#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <objbase.h>
#include <oleauto.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

#include "decklink_shim.h"
#include "DeckLinkAPI.h"
#include "DeckLinkAPI_i.c"

namespace {

struct ComScope {
    bool should_uninit = false;
    ComScope() {
        HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        if (hr == S_OK || hr == S_FALSE) {
            should_uninit = true;
        }
    }
    ~ComScope() {
        if (should_uninit) {
            CoUninitialize();
        }
    }
};

static std::string bstr_to_utf8(BSTR bstr) {
    if (!bstr) return {};
    UINT wlen = SysStringLen(bstr);
    if (wlen == 0) return {};
    int needed = WideCharToMultiByte(CP_UTF8, 0, bstr, (int)wlen, nullptr, 0, nullptr, nullptr);
    if (needed <= 0) return {};
    std::string out((size_t)needed, '\0');
    WideCharToMultiByte(CP_UTF8, 0, bstr, (int)wlen, out.data(), needed, nullptr, nullptr);
    return out;
}

static void write_err(char* buf, uint32_t cap, const char* msg) {
    if (!buf || cap == 0) return;
    if (!msg) msg = "";
    std::snprintf(buf, cap, "%s", msg);
    buf[cap - 1] = '\0';
}

static std::string format_mode_status(long width, long height, BMDTimeValue frameDuration, BMDTimeScale timeScale, BMDFieldDominance fieldDom) {
    double fps = (frameDuration > 0) ? ((double)timeScale / (double)frameDuration) : 0.0;
    const char* scan = (fieldDom == bmdLowerFieldFirst || fieldDom == bmdUpperFieldFirst) ? "i" : "p";
    char rate_buf[32];
    double rounded = (double)((long long)(fps + 0.5));
    if (fps > 0.0 && (fps - rounded < 0.005 && rounded - fps < 0.005)) {
        std::snprintf(rate_buf, sizeof(rate_buf), "%lld", (long long)rounded);
    } else {
        std::snprintf(rate_buf, sizeof(rate_buf), "%.2f", fps);
    }
    char buf[128];
    std::snprintf(buf, sizeof(buf), "DeckLink %ldx%ld %s%s", width, height, rate_buf, scan);
    return std::string(buf);
}

class InputCallback;

} // namespace

struct DlCapture {
    IDeckLink* decklink = nullptr;
    IDeckLinkInput* input = nullptr;
    InputCallback* callback = nullptr;

    DlFrameCallback user_cb = nullptr;
    void* user_ptr = nullptr;

    std::atomic<bool> stopped{false};

    std::mutex state_mu;
    std::string status_str = "신호 없음";
    std::string mode_status_str = "DeckLink 1920x1080 60p";
    int32_t fps_n = 60000;
    int32_t fps_d = 1000;

    void set_status(const std::string& s) {
        std::lock_guard<std::mutex> lk(state_mu);
        status_str = s;
    }

    void update_mode_info(IDeckLinkDisplayMode* mode) {
        if (!mode) return;
        long w = mode->GetWidth();
        long h = mode->GetHeight();
        BMDTimeValue dur = 1000;
        BMDTimeScale scale = 60000;
        if (FAILED(mode->GetFrameRate(&dur, &scale)) || dur <= 0 || scale <= 0) {
            dur = 1000;
            scale = 60000;
        }
        BMDFieldDominance dom = mode->GetFieldDominance();
        std::string ms = format_mode_status(w, h, dur, scale, dom);

        // Reduce scale/dur to fit comfortably in int32_t if needed
        while ((scale > 0x7fffffffLL || dur > 0x7fffffffLL) && dur > 1) {
            scale >>= 1;
            dur >>= 1;
        }

        std::lock_guard<std::mutex> lk(state_mu);
        fps_n = (int32_t)scale;
        fps_d = (int32_t)dur;
        mode_status_str = ms;
    }
};

namespace {

class InputCallback final : public IDeckLinkInputCallback {
public:
    explicit InputCallback(DlCapture* owner) : ref_count_(1), owner_(owner) {}
    virtual ~InputCallback() = default;

    void Detach() {
        std::lock_guard<std::mutex> lk(cb_mu_);
        owner_ = nullptr;
    }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, LPVOID* ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (IsEqualGUID(iid, IID_IUnknown) || IsEqualGUID(iid, IID_IDeckLinkInputCallback)) {
            *ppv = static_cast<IDeckLinkInputCallback*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    ULONG STDMETHODCALLTYPE AddRef(void) override {
        return (ULONG)InterlockedIncrement(&ref_count_);
    }

    ULONG STDMETHODCALLTYPE Release(void) override {
        ULONG r = (ULONG)InterlockedDecrement(&ref_count_);
        if (r == 0) {
            delete this;
        }
        return r;
    }

    HRESULT STDMETHODCALLTYPE VideoInputFormatChanged(
        BMDVideoInputFormatChangedEvents /*notificationEvents*/,
        IDeckLinkDisplayMode* newDisplayMode,
        BMDDetectedVideoInputFormatFlags /*detectedSignalFlags*/
    ) override {
        std::lock_guard<std::mutex> lk(cb_mu_);
        if (!owner_ || owner_->stopped.load(std::memory_order_acquire) || !owner_->input || !newDisplayMode) {
            return S_OK;
        }

        BMDDisplayMode mode_id = newDisplayMode->GetDisplayMode();
        owner_->update_mode_info(newDisplayMode);

        // Always keep 8-bit YUV (bmdFormat8BitYUV / UYVY 4:2:2) even if detected signal is RGB:
        // DeckLink hardware converts RGB input to 8-bit YUV when bmdFormat8BitYUV is requested.
        owner_->input->PauseStreams();
        HRESULT hr = owner_->input->EnableVideoInput(
            mode_id,
            bmdFormat8BitYUV,
            bmdVideoInputEnableFormatDetection
        );
        owner_->input->FlushStreams();
        if (SUCCEEDED(hr)) {
            owner_->input->StartStreams();
        }
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE VideoInputFrameArrived(
        IDeckLinkVideoInputFrame* videoFrame,
        IDeckLinkAudioInputPacket* /*audioPacket*/
    ) override {
        std::lock_guard<std::mutex> lk(cb_mu_);
        if (!owner_ || owner_->stopped.load(std::memory_order_acquire) || !videoFrame) {
            return S_OK;
        }

        BMDFrameFlags flags = videoFrame->GetFlags();
        if ((flags & bmdFrameHasNoInputSource) != 0) {
            owner_->set_status("신호 없음");
            if (owner_->user_cb) {
                owner_->user_cb(owner_->user_ptr, nullptr, 0, 0, 0, 0, 0, 0);
            }
            return S_OK;
        }

        void* bytes = nullptr;
        if (FAILED(videoFrame->GetBytes(&bytes)) || !bytes) {
            return S_OK;
        }

        long width = videoFrame->GetWidth();
        long height = videoFrame->GetHeight();
        long row_bytes = videoFrame->GetRowBytes();
        if (width <= 0 || height <= 0 || row_bytes < width * 2) {
            return S_OK;
        }

        int32_t fn = 60000;
        int32_t fd = 1000;
        {
            std::lock_guard<std::mutex> slk(owner_->state_mu);
            fn = owner_->fps_n;
            fd = owner_->fps_d;
            owner_->status_str = owner_->mode_status_str;
        }

        BMDTimeValue stream_time = 0;
        BMDTimeValue frame_dur = 0;
        if (fn > 0 && SUCCEEDED(videoFrame->GetStreamTime(&stream_time, &frame_dur, (BMDTimeScale)fn)) && frame_dur > 0 && frame_dur <= 0x7fffffffLL) {
            fd = (int32_t)frame_dur;
        }

        if (owner_->user_cb) {
            owner_->user_cb(
                owner_->user_ptr,
                static_cast<const uint8_t*>(bytes),
                (uint32_t)width,
                (uint32_t)height,
                (uint32_t)row_bytes,
                fn,
                fd,
                1
            );
        }
        return S_OK;
    }

private:
    volatile LONG ref_count_;
    std::mutex cb_mu_;
    DlCapture* owner_;
};

} // namespace

extern "C" int32_t dl_list_devices(void (*cb)(void* user, const char* name), void* user) {
    ComScope com;

    IDeckLinkIterator* iter = nullptr;
    HRESULT hr = CoCreateInstance(
        CLSID_CDeckLinkIterator,
        nullptr,
        CLSCTX_ALL,
        IID_IDeckLinkIterator,
        reinterpret_cast<void**>(&iter)
    );
    if (FAILED(hr) || !iter) {
        return 0;
    }

    int32_t count = 0;
    IDeckLink* dl = nullptr;
    while (iter->Next(&dl) == S_OK && dl != nullptr) {
        BSTR bname = nullptr;
        std::string name;
        if (SUCCEEDED(dl->GetDisplayName(&bname)) && bname) {
            name = bstr_to_utf8(bname);
            SysFreeString(bname);
        } else if (SUCCEEDED(dl->GetModelName(&bname)) && bname) {
            name = bstr_to_utf8(bname);
            SysFreeString(bname);
        }
        if (name.empty()) {
            char fallback[64];
            std::snprintf(fallback, sizeof(fallback), "DeckLink #%d", (int)count);
            name = fallback;
        }
        if (cb) {
            cb(user, name.c_str());
        }
        dl->Release();
        dl = nullptr;
        ++count;
    }

    iter->Release();
    return count;
}

extern "C" DlCapture* dl_start(
    uint32_t index,
    DlFrameCallback cb,
    void* user,
    char* err_buf,
    uint32_t err_cap
) {
    ComScope com;

    IDeckLinkIterator* iter = nullptr;
    HRESULT hr = CoCreateInstance(
        CLSID_CDeckLinkIterator,
        nullptr,
        CLSCTX_ALL,
        IID_IDeckLinkIterator,
        reinterpret_cast<void**>(&iter)
    );
    if (FAILED(hr) || !iter) {
        write_err(err_buf, err_cap, "Blackmagic Desktop Video 드라이버가 없습니다");
        return nullptr;
    }

    IDeckLink* target = nullptr;
    IDeckLink* cur = nullptr;
    uint32_t idx = 0;
    while (iter->Next(&cur) == S_OK && cur != nullptr) {
        if (idx == index) {
            target = cur;
            break;
        }
        cur->Release();
        cur = nullptr;
        ++idx;
    }
    iter->Release();

    if (!target) {
        write_err(err_buf, err_cap, "선택한 DeckLink 장치를 찾을 수 없습니다");
        return nullptr;
    }

    IDeckLinkInput* input = nullptr;
    hr = target->QueryInterface(IID_IDeckLinkInput, reinterpret_cast<void**>(&input));
    if (FAILED(hr) || !input) {
        target->Release();
        write_err(err_buf, err_cap, "DeckLink 장치가 비디오 입력(IDeckLinkInput)을 지원하지 않습니다");
        return nullptr;
    }

    BOOL supports_fmt_detect = TRUE;
    IDeckLinkProfileAttributes* attrs = nullptr;
    if (SUCCEEDED(target->QueryInterface(IID_IDeckLinkProfileAttributes, reinterpret_cast<void**>(&attrs))) && attrs) {
        BOOL flag = FALSE;
        if (SUCCEEDED(attrs->GetFlag(BMDDeckLinkSupportsInputFormatDetection, &flag))) {
            supports_fmt_detect = flag;
        }
        attrs->Release();
    }

    BMDVideoInputFlags input_flags = supports_fmt_detect ? bmdVideoInputEnableFormatDetection : bmdVideoInputFlagDefault;
    BMDDisplayMode chosen_mode = bmdModeHD1080p6000;
    IDeckLinkDisplayMode* mode_obj = nullptr;

    BMDDisplayMode candidate_modes[] = {
        bmdModeHD1080p6000,
        bmdModeHD1080p5994,
        bmdModeHD1080p30,
        bmdModeHD1080p2997,
        bmdModeHD1080i5994,
        bmdModeHD720p60,
        bmdModeHD720p5994,
        bmdModeNTSC,
        bmdModePAL
    };

    bool found_mode = false;
    for (BMDDisplayMode m : candidate_modes) {
        BOOL supported = FALSE;
        BMDDisplayMode actual = m;
        if (SUCCEEDED(input->DoesSupportVideoMode(
                bmdVideoConnectionUnspecified,
                m,
                bmdFormat8BitYUV,
                bmdNoVideoInputConversion,
                bmdSupportedVideoModeDefault,
                &actual,
                &supported)) && supported) {
            chosen_mode = actual;
            input->GetDisplayMode(chosen_mode, &mode_obj);
            found_mode = true;
            break;
        }
    }

    if (!found_mode) {
        IDeckLinkDisplayModeIterator* mode_iter = nullptr;
        if (SUCCEEDED(input->GetDisplayModeIterator(&mode_iter)) && mode_iter) {
            IDeckLinkDisplayMode* dm = nullptr;
            if (mode_iter->Next(&dm) == S_OK && dm) {
                chosen_mode = dm->GetDisplayMode();
                mode_obj = dm;
                found_mode = true;
            }
            mode_iter->Release();
        }
    }

    auto* cap = new DlCapture();
    cap->decklink = target;
    cap->input = input;
    cap->user_cb = cb;
    cap->user_ptr = user;
    if (mode_obj) {
        cap->update_mode_info(mode_obj);
        mode_obj->Release();
        mode_obj = nullptr;
    }

    cap->callback = new InputCallback(cap);
    hr = input->SetCallback(cap->callback);
    if (FAILED(hr)) {
        dl_destroy(cap);
        write_err(err_buf, err_cap, "DeckLink 입력 콜백 설정 실패");
        return nullptr;
    }

    hr = input->EnableVideoInput(chosen_mode, bmdFormat8BitYUV, input_flags);
    if (FAILED(hr) && input_flags != bmdVideoInputFlagDefault) {
        hr = input->EnableVideoInput(chosen_mode, bmdFormat8BitYUV, bmdVideoInputFlagDefault);
    }
    if (FAILED(hr)) {
        dl_destroy(cap);
        write_err(err_buf, err_cap, "DeckLink 비디오 입력 활성화(EnableVideoInput) 실패");
        return nullptr;
    }

    hr = input->StartStreams();
    if (FAILED(hr)) {
        dl_destroy(cap);
        write_err(err_buf, err_cap, "DeckLink 스트림 시작(StartStreams) 실패");
        return nullptr;
    }

    return cap;
}

extern "C" void dl_stop(DlCapture* cap) {
    if (!cap) return;
    bool already = cap->stopped.exchange(true, std::memory_order_acq_rel);
    if (already) return;

    ComScope com;

    if (cap->callback) {
        cap->callback->Detach();
    }
    if (cap->input) {
        cap->input->StopStreams();
        cap->input->SetCallback(nullptr);
        cap->input->DisableVideoInput();
        cap->input->Release();
        cap->input = nullptr;
    }
    if (cap->callback) {
        cap->callback->Release();
        cap->callback = nullptr;
    }
    if (cap->decklink) {
        cap->decklink->Release();
        cap->decklink = nullptr;
    }
}

extern "C" void dl_destroy(DlCapture* cap) {
    if (!cap) return;
    dl_stop(cap);
    delete cap;
}

extern "C" uint32_t dl_status(DlCapture* cap, char* buf, uint32_t cap_len) {
    if (!cap || !buf || cap_len == 0) return 0;
    std::string s;
    {
        std::lock_guard<std::mutex> lk(cap->state_mu);
        s = cap->status_str;
    }
    uint32_t copy_len = (s.size() < (size_t)(cap_len - 1)) ? (uint32_t)s.size() : (cap_len - 1);
    if (copy_len > 0) {
        std::memcpy(buf, s.data(), copy_len);
    }
    buf[copy_len] = '\0';
    return copy_len;
}
