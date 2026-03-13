/**
 * Video thumbnail extractor using FFmpeg C API.
 * Extracts a single frame from a video file and returns it as raw RGBA pixels.
 * Called from Rust via FFI.
 */

#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libswscale/swscale.h>
#include <libavutil/imgutils.h>
#include <stdlib.h>
#include <string.h>

/**
 * Extract a video frame as RGBA pixels.
 *
 * @param path       Path to the video file (UTF-8)
 * @param max_size   Maximum thumbnail dimension
 * @param out_data   Output: malloc'd RGBA pixel buffer (caller must free)
 * @param out_width  Output: actual thumbnail width
 * @param out_height Output: actual thumbnail height
 * @return 0 on success, negative on error
 */
int extract_video_frame(
    const char* path,
    int max_size,
    unsigned char** out_data,
    int* out_width,
    int* out_height
) {
    AVFormatContext* fmt_ctx = NULL;
    AVCodecContext* codec_ctx = NULL;
    struct SwsContext* sws_ctx = NULL;
    AVFrame* frame = NULL;
    AVFrame* rgb_frame = NULL;
    AVPacket* pkt = NULL;
    int ret = -1;
    int video_stream = -1;

    *out_data = NULL;
    *out_width = 0;
    *out_height = 0;

    /* Open file */
    if (avformat_open_input(&fmt_ctx, path, NULL, NULL) < 0)
        return -1;

    if (avformat_find_stream_info(fmt_ctx, NULL) < 0)
        goto cleanup;

    /* Find video stream */
    for (unsigned i = 0; i < fmt_ctx->nb_streams; i++) {
        if (fmt_ctx->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_VIDEO) {
            video_stream = (int)i;
            break;
        }
    }
    if (video_stream < 0)
        goto cleanup;

    /* Open decoder */
    const AVCodec* codec = avcodec_find_decoder(
        fmt_ctx->streams[video_stream]->codecpar->codec_id
    );
    if (!codec)
        goto cleanup;

    codec_ctx = avcodec_alloc_context3(codec);
    if (!codec_ctx)
        goto cleanup;

    if (avcodec_parameters_to_context(codec_ctx, fmt_ctx->streams[video_stream]->codecpar) < 0)
        goto cleanup;

    if (avcodec_open2(codec_ctx, codec, NULL) < 0)
        goto cleanup;

    /* Calculate output dimensions preserving aspect ratio */
    int src_w = codec_ctx->width;
    int src_h = codec_ctx->height;
    int dst_w, dst_h;

    if (src_w <= 0 || src_h <= 0)
        goto cleanup;

    if (src_w >= src_h) {
        dst_w = max_size;
        dst_h = (int)((float)max_size * src_h / src_w);
    } else {
        dst_h = max_size;
        dst_w = (int)((float)max_size * src_w / src_h);
    }
    if (dst_w <= 0) dst_w = 1;
    if (dst_h <= 0) dst_h = 1;

    /* Create scaler */
    sws_ctx = sws_getContext(
        src_w, src_h, codec_ctx->pix_fmt,
        dst_w, dst_h, AV_PIX_FMT_RGBA,
        SWS_BILINEAR, NULL, NULL, NULL
    );
    if (!sws_ctx)
        goto cleanup;

    frame = av_frame_alloc();
    rgb_frame = av_frame_alloc();
    pkt = av_packet_alloc();
    if (!frame || !rgb_frame || !pkt)
        goto cleanup;

    /* Allocate output buffer */
    int buf_size = av_image_get_buffer_size(AV_PIX_FMT_RGBA, dst_w, dst_h, 1);
    unsigned char* buffer = (unsigned char*)av_malloc(buf_size);
    if (!buffer)
        goto cleanup;

    av_image_fill_arrays(
        rgb_frame->data, rgb_frame->linesize,
        buffer, AV_PIX_FMT_RGBA, dst_w, dst_h, 1
    );

    /* Try seeking to 1s, then 0s */
    int64_t seek_targets[] = { 1 * AV_TIME_BASE, 0 };
    int num_seeks = 2;
    int decoded = 0;

    for (int s = 0; s < num_seeks && !decoded; s++) {
        av_seek_frame(fmt_ctx, -1, seek_targets[s], AVSEEK_FLAG_BACKWARD);
        avcodec_flush_buffers(codec_ctx);

        int frames_read = 0;
        while (frames_read < 100) {
            int rd = av_read_frame(fmt_ctx, pkt);
            if (rd < 0) break;

            if (pkt->stream_index != video_stream) {
                av_packet_unref(pkt);
                continue;
            }

            if (avcodec_send_packet(codec_ctx, pkt) < 0) {
                av_packet_unref(pkt);
                continue;
            }
            av_packet_unref(pkt);

            if (avcodec_receive_frame(codec_ctx, frame) == 0) {
                /* Scale frame to RGBA */
                sws_scale(sws_ctx,
                    (const uint8_t* const*)frame->data, frame->linesize,
                    0, src_h,
                    rgb_frame->data, rgb_frame->linesize
                );
                decoded = 1;
                break;
            }
            frames_read++;
        }
    }

    if (decoded) {
        /* Copy to output buffer */
        int data_size = dst_w * dst_h * 4;
        *out_data = (unsigned char*)malloc(data_size);
        if (*out_data) {
            /* Copy row by row in case of padding */
            for (int y = 0; y < dst_h; y++) {
                memcpy(
                    *out_data + y * dst_w * 4,
                    rgb_frame->data[0] + y * rgb_frame->linesize[0],
                    dst_w * 4
                );
            }
            *out_width = dst_w;
            *out_height = dst_h;
            ret = 0;
        }
    }

cleanup:
    if (buffer) av_free(buffer);
    if (pkt) av_packet_free(&pkt);
    if (rgb_frame) av_frame_free(&rgb_frame);
    if (frame) av_frame_free(&frame);
    if (sws_ctx) sws_freeContext(sws_ctx);
    if (codec_ctx) avcodec_free_context(&codec_ctx);
    if (fmt_ctx) avformat_close_input(&fmt_ctx);
    return ret;
}

/**
 * Free a buffer returned by extract_video_frame.
 */
void free_frame_data(unsigned char* data) {
    free(data);
}
