#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>

typedef struct LavReader {
    AVFormatContext *fmt;
    AVCodecContext *dec;
    AVFrame *frame;
    AVPacket *pkt;
    SwsContext *sws;
    AVFrame *dst;
    AVFrame *rgb;
    int stream;
    int draining;
    int out_w, out_h;
    int has_alpha;
    char codec[64];
} LavReader;

static void set_err(char *err, int cap, const char *what, int code) {
    char buf[128] = {0};
    if (code) av_strerror(code, buf, sizeof(buf));
    if (err && cap > 0) snprintf(err, (size_t)cap, "%s%s%s", what, code ? ": " : "", buf);
}

static int src_has_alpha(enum AVPixelFormat f) {
    const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(f);
    return d && (d->flags & AV_PIX_FMT_FLAG_ALPHA);
}

static int src_is_rgb(enum AVPixelFormat f) {
    const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(f);
    return d && (d->flags & AV_PIX_FMT_FLAG_RGB);
}

void lav_close(LavReader *r) {
    if (!r) return;
    if (r->sws) sws_free_context(&r->sws);
    if (r->dst) av_frame_free(&r->dst);
    if (r->rgb) av_frame_free(&r->rgb);
    if (r->frame) av_frame_free(&r->frame);
    if (r->pkt) av_packet_free(&r->pkt);
    if (r->dec) avcodec_free_context(&r->dec);
    if (r->fmt) avformat_close_input(&r->fmt);
    av_free(r);
}

LavReader *lav_open(const char *utf8_path, char *err, int errcap, int *w, int *h, int *fps_n, int *fps_d, int *has_alpha,
                    char *codec_out, int codec_cap) {
    LavReader *r = av_mallocz(sizeof(LavReader));
    if (!r) { set_err(err, errcap, "메모리 부족", 0); return NULL; }
    int ret = avformat_open_input(&r->fmt, utf8_path, NULL, NULL);
    if (ret < 0) { set_err(err, errcap, "파일을 열 수 없습니다", ret); lav_close(r); return NULL; }
    ret = avformat_find_stream_info(r->fmt, NULL);
    if (ret < 0) { set_err(err, errcap, "영상 정보를 읽을 수 없습니다", ret); lav_close(r); return NULL; }
    const AVCodec *codec = NULL;
    r->stream = av_find_best_stream(r->fmt, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    if (r->stream < 0 || !codec) { set_err(err, errcap, "영상 트랙이 없거나 지원하지 않는 코덱입니다", r->stream < 0 ? r->stream : 0); lav_close(r); return NULL; }
    AVStream *st = r->fmt->streams[r->stream];
    r->dec = avcodec_alloc_context3(codec);
    if (!r->dec) { set_err(err, errcap, "디코더 생성 실패", 0); lav_close(r); return NULL; }
    avcodec_parameters_to_context(r->dec, st->codecpar);
    r->dec->thread_count = 0;
    r->dec->thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
    ret = avcodec_open2(r->dec, codec, NULL);
    if (ret < 0) { set_err(err, errcap, "디코더를 열 수 없습니다", ret); lav_close(r); return NULL; }
    r->frame = av_frame_alloc();
    r->pkt = av_packet_alloc();
    if (!r->frame || !r->pkt) { set_err(err, errcap, "메모리 부족", 0); lav_close(r); return NULL; }
    r->out_w = r->dec->width & ~1;
    r->out_h = r->dec->height;
    if (r->out_w < 2 || r->out_h < 2) { set_err(err, errcap, "영상 크기를 알 수 없습니다", 0); lav_close(r); return NULL; }
    r->has_alpha = src_has_alpha(r->dec->pix_fmt);
    AVRational fr = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
    if (fr.num <= 0 || fr.den <= 0) { fr.num = 30; fr.den = 1; }
    snprintf(r->codec, sizeof(r->codec), "%s", codec->name);
    *w = r->out_w; *h = r->out_h; *fps_n = fr.num; *fps_d = fr.den; *has_alpha = r->has_alpha;
    if (codec_out && codec_cap > 0) snprintf(codec_out, (size_t)codec_cap, "%s", codec->name);
    return r;
}

static inline uint8_t clip8(int v) { return (uint8_t)(v < 0 ? 0 : v > 255 ? 255 : v); }

static AVFrame *ycgco_to_gbrap(LavReader *r, AVFrame *src) {
    const AVPixFmtDescriptor *d = av_pix_fmt_desc_get((enum AVPixelFormat)src->format);
    if (!d || (d->flags & (AV_PIX_FMT_FLAG_RGB | AV_PIX_FMT_FLAG_BE)) || d->comp[0].depth != 8 || d->nb_components < 3) return NULL;
    int has_a = d->nb_components == 4;
    int sx = d->log2_chroma_w, sy = d->log2_chroma_h;
    if (!r->rgb) r->rgb = av_frame_alloc();
    if (!r->rgb) return NULL;
    AVFrame *o = r->rgb;
    if (o->width != src->width || o->height != src->height || !o->data[0]) {
        av_frame_unref(o);
        o->width = src->width;
        o->height = src->height;
        o->format = AV_PIX_FMT_GBRAP;
        if (av_frame_get_buffer(o, 32) < 0) return NULL;
    }
    o->color_range = AVCOL_RANGE_JPEG;
    o->colorspace = AVCOL_SPC_RGB;
    int lim = src->color_range == AVCOL_RANGE_MPEG;
    for (int y = 0; y < src->height; y++) {
        const uint8_t *Y = src->data[0] + (size_t)y * src->linesize[0];
        const uint8_t *Cg = src->data[1] + (size_t)(y >> sy) * src->linesize[1];
        const uint8_t *Co = src->data[2] + (size_t)(y >> sy) * src->linesize[2];
        const uint8_t *A = has_a ? src->data[3] + (size_t)y * src->linesize[3] : NULL;
        uint8_t *G = o->data[0] + (size_t)y * o->linesize[0];
        uint8_t *B = o->data[1] + (size_t)y * o->linesize[1];
        uint8_t *R = o->data[2] + (size_t)y * o->linesize[2];
        uint8_t *OA = o->data[3] + (size_t)y * o->linesize[3];
        for (int x = 0; x < src->width; x++) {
            int yy = Y[x], cg = Cg[x >> sx] - 128, co = Co[x >> sx] - 128;
            if (lim) { yy = (yy - 16) * 255 / 219; cg = cg * 255 / 224; co = co * 255 / 224; }
            int t = yy - cg;
            G[x] = clip8(yy + cg);
            B[x] = clip8(t - co);
            R[x] = clip8(t + co);
            OA[x] = A ? A[x] : 255;
        }
    }
    return o;
}

static int convert(LavReader *r, uint8_t *uyvy, uint8_t *alpha, int *got_alpha, char *err, int errcap) {
    AVFrame *src = r->frame;
    if (src->colorspace == AVCOL_SPC_YCGCO) {
        AVFrame *g = ycgco_to_gbrap(r, src);
        if (!g) { set_err(err, errcap, "YCgCo 색공간 변환 실패", 0); return -1; }
        src = g;
    }
    int rgb = src_is_rgb((enum AVPixelFormat)src->format);
    int want_alpha = alpha != NULL && src_has_alpha((enum AVPixelFormat)src->format);
    if (!r->sws) {
        r->sws = sws_alloc_context();
        if (!r->sws) { set_err(err, errcap, "변환기 생성 실패", 0); return -1; }
        r->sws->threads = 0;
        r->sws->flags = SWS_BILINEAR;
    }
    if (!r->dst) {
        r->dst = av_frame_alloc();
        if (!r->dst) { set_err(err, errcap, "메모리 부족", 0); return -1; }
    }
    if (src->color_range == AVCOL_RANGE_UNSPECIFIED) src->color_range = rgb ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
    if (!rgb && src->colorspace == AVCOL_SPC_UNSPECIFIED) src->colorspace = src->height < 720 ? AVCOL_SPC_SMPTE170M : AVCOL_SPC_BT709;
    AVFrame *d = r->dst;
    av_frame_unref(d);
    d->width = r->out_w;
    d->height = r->out_h;
    d->format = want_alpha ? AV_PIX_FMT_YUVA422P : AV_PIX_FMT_UYVY422;
    d->colorspace = AVCOL_SPC_BT709;
    d->color_range = AVCOL_RANGE_MPEG;
    d->color_primaries = src->color_primaries;
    d->color_trc = src->color_trc;
    if (!want_alpha) {
        d->data[0] = uyvy;
        d->linesize[0] = r->out_w * 2;
    }
    int ret = sws_scale_frame(r->sws, d, src);
    if (ret < 0) { set_err(err, errcap, "픽셀 형식 변환 실패", ret); return -1; }
    if (!want_alpha) {
        if (d->data[0] != uyvy) {
            for (int y = 0; y < r->out_h; y++) memcpy(uyvy + (size_t)y * r->out_w * 2, d->data[0] + (size_t)y * d->linesize[0], (size_t)r->out_w * 2);
        }
        *got_alpha = 0;
        return 0;
    }
    int w = r->out_w, h = r->out_h;
    for (int y = 0; y < h; y++) {
        const uint8_t *Y = d->data[0] + (size_t)y * d->linesize[0];
        const uint8_t *U = d->data[1] + (size_t)y * d->linesize[1];
        const uint8_t *V = d->data[2] + (size_t)y * d->linesize[2];
        const uint8_t *A = d->data[3] + (size_t)y * d->linesize[3];
        uint8_t *o = uyvy + (size_t)y * w * 2;
        for (int x = 0; x < w / 2; x++) {
            o[4 * x + 0] = U[x];
            o[4 * x + 1] = Y[2 * x];
            o[4 * x + 2] = V[x];
            o[4 * x + 3] = Y[2 * x + 1];
        }
        memcpy(alpha + (size_t)y * w, A, (size_t)w);
    }
    *got_alpha = 1;
    return 0;
}

int lav_read(LavReader *r, uint8_t *uyvy, uint8_t *alpha, int *got_alpha, int64_t *pts_100ns, char *err, int errcap) {
    for (;;) {
        int ret = avcodec_receive_frame(r->dec, r->frame);
        if (ret == 0) {
            int64_t ts = r->frame->best_effort_timestamp;
            if (ts == AV_NOPTS_VALUE) ts = r->frame->pts;
            AVRational tb = r->fmt->streams[r->stream]->time_base;
            *pts_100ns = ts == AV_NOPTS_VALUE ? -1 : av_rescale_q(ts, tb, (AVRational){1, 10000000});
            int c = convert(r, uyvy, alpha, got_alpha, err, errcap);
            av_frame_unref(r->frame);
            return c < 0 ? -1 : 1;
        }
        if (ret == AVERROR_EOF) return 0;
        if (ret != AVERROR(EAGAIN)) { set_err(err, errcap, "영상 디코딩 오류", ret); return -1; }
        if (r->draining) return 0;
        ret = av_read_frame(r->fmt, r->pkt);
        if (ret < 0) {
            r->draining = 1;
            avcodec_send_packet(r->dec, NULL);
            continue;
        }
        if (r->pkt->stream_index == r->stream) {
            ret = avcodec_send_packet(r->dec, r->pkt);
            if (ret < 0 && ret != AVERROR(EAGAIN)) { av_packet_unref(r->pkt); set_err(err, errcap, "영상 디코딩 오류", ret); return -1; }
        }
        av_packet_unref(r->pkt);
    }
}

int lav_rewind(LavReader *r) {
    int ret = av_seek_frame(r->fmt, -1, 0, AVSEEK_FLAG_BACKWARD);
    avcodec_flush_buffers(r->dec);
    r->draining = 0;
    return ret;
}
