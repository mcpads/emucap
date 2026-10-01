#include <gccore.h>
#include <malloc.h>
#include <ogc/lwp_watchdog.h>
#include <wiiuse/wpad.h>

#ifndef FIXTURE_USE_GX
#define FIXTURE_USE_GX 0
#endif

#ifndef FIXTURE_TEXTURE
#define FIXTURE_TEXTURE 0
#endif

#ifndef FIXTURE_TEXTURE_COPY
#define FIXTURE_TEXTURE_COPY 1
#endif

#ifndef FIXTURE_CLEAR_EFB
#define FIXTURE_CLEAR_EFB 1
#endif

#ifndef FIXTURE_DEPTH
#define FIXTURE_DEPTH 0
#endif

#ifndef FIXTURE_GX_PARTS
#define FIXTURE_GX_PARTS 1
#endif

#ifndef FIXTURE_RETRACES_PER_IMAGE
#define FIXTURE_RETRACES_PER_IMAGE 1
#endif
#if FIXTURE_RETRACES_PER_IMAGE != 1 && FIXTURE_RETRACES_PER_IMAGE != 2
#error "FIXTURE_RETRACES_PER_IMAGE must be 1 or 2"
#endif

/* Guest observations, read only after a native instruction halt. */
volatile u32 fixture_record[14];
static volatile u32 fixture_retraces;
static void on_retrace(u32 count) { fixture_retraces = count; }
__attribute__((noinline)) void fixture_observe(void) {
    __asm__ volatile ("" ::: "memory");
}
int main(void) {
    VIDEO_Init();
    VIDEO_SetPostRetraceCallback(on_retrace);
    GXRModeObj *mode = &TVNtsc480Prog;
    volatile u32 *xfb = MEM_K0_TO_K1(SYS_AllocateFramebuffer(mode));
    VIDEO_Configure(mode);
    VIDEO_SetNextFramebuffer((void *)xfb);
    VIDEO_ClearFrameBuffer(mode, (void *)xfb, COLOR_BLACK);
    VIDEO_SetBlack(false);
    VIDEO_Flush();
    VIDEO_WaitVSync();
#if FIXTURE_USE_GX
    void *fifo = memalign(32, 256 * 1024);
    if (!fifo) return 1;
    GX_Init(fifo, 256 * 1024);
    GX_SetDispCopySrc(0, 0, mode->fbWidth, mode->efbHeight / FIXTURE_GX_PARTS);
    GX_SetDispCopyDst(mode->fbWidth, mode->xfbHeight / FIXTURE_GX_PARTS);
    GX_SetDispCopyYScale(GX_GetYScaleFactor(mode->efbHeight / FIXTURE_GX_PARTS,
                                             mode->xfbHeight / FIXTURE_GX_PARTS));
    GX_SetCopyFilter(mode->aa, mode->sample_pattern, GX_TRUE, mode->vfilter);
    GX_SetPixelFmt(GX_PF_RGB8_Z24, GX_ZC_LINEAR);
    GX_SetDispCopyGamma(GX_GM_1_0);
#endif
#if FIXTURE_TEXTURE
    void *texels = memalign(32, 32 * 32 * 2);
    if (!texels) return 1;
#if !FIXTURE_TEXTURE_COPY
    for (u32 i = 0; i < 32 * 32; ++i) ((u16 *)texels)[i] = 0xf800;
    DCFlushRange(texels, 32 * 32 * 2);
#endif
    GXTexObj texture;
    GX_InitTexObj(&texture, texels, 32, 32, GX_TF_RGB565, GX_CLAMP, GX_CLAMP, GX_FALSE);
    GX_SetViewport(0, 0, mode->fbWidth, mode->efbHeight, 0, 1);
    GX_SetScissor(0, 0, mode->fbWidth, mode->efbHeight);
    Mtx44 projection;
    Mtx model;
    guOrtho(projection, -1, 1, -1, 1, 0, 1);
    guMtxIdentity(model);
    GX_LoadProjectionMtx(projection, GX_ORTHOGRAPHIC);
    GX_LoadPosMtxImm(model, GX_PNMTX0);
    GX_SetCurrentMtx(GX_PNMTX0);
    GX_SetCullMode(GX_CULL_NONE);
#if FIXTURE_DEPTH
    GX_SetZMode(GX_TRUE, GX_LEQUAL, GX_TRUE);
#else
    GX_SetZMode(GX_FALSE, GX_ALWAYS, GX_FALSE);
#endif
    GX_SetColorUpdate(GX_TRUE);
    GX_SetNumChans(0);
    GX_SetNumTexGens(1);
    GX_SetTexCoordGen(GX_TEXCOORD0, GX_TG_MTX2x4, GX_TG_TEX0, GX_IDENTITY);
    GX_SetNumTevStages(1);
    GX_SetTevOrder(GX_TEVSTAGE0, GX_TEXCOORD0, GX_TEXMAP0, GX_COLORNULL);
    GX_SetTevOp(GX_TEVSTAGE0, GX_REPLACE);
    GX_ClearVtxDesc();
    GX_SetVtxDesc(GX_VA_POS, GX_DIRECT);
    GX_SetVtxDesc(GX_VA_TEX0, GX_DIRECT);
    GX_SetVtxAttrFmt(GX_VTXFMT0, GX_VA_POS, GX_POS_XYZ, GX_F32, 0);
    GX_SetVtxAttrFmt(GX_VTXFMT0, GX_VA_TEX0, GX_TEX_ST, GX_F32, 0);
#endif
    WPAD_Init();
    WPAD_SetDataFormat(WPAD_CHAN_0, WPAD_FMT_BTNS);
    fixture_record[0] = 0x57504144;
    fixture_record[1] = 1;
    for (;;) {
        VIDEO_WaitVSync();
        WPAD_ScanPads();
        s32 probe = WPAD_Probe(WPAD_CHAN_0, NULL);
        u32 buttons = WPAD_ButtonsHeld(WPAD_CHAN_0);
        u32 down = WPAD_ButtonsDown(WPAD_CHAN_0);
        u32 up = WPAD_ButtonsUp(WPAD_CHAN_0);
        u64 ticks = gettime();
        fixture_record[2]++;
        if (probe == WPAD_ERR_NONE) {
            fixture_record[3]++;
            fixture_record[4] += !!(buttons & WPAD_BUTTON_A);
            fixture_record[5] += !!(down & WPAD_BUTTON_A);
            fixture_record[6] += !!(up & WPAD_BUTTON_A);
            fixture_record[13]++;
        } else fixture_record[13] = 0;
        fixture_record[7] = buttons;
        fixture_record[8] = (u32)probe;
        fixture_record[9] = (u32)(ticks >> 32);
        fixture_record[10] = (u32)ticks;
        fixture_record[11] = fixture_retraces;
        fixture_record[12] = WPAD_Data(WPAD_CHAN_0)->data_present;
#if FIXTURE_USE_GX
        if (fixture_record[2] % FIXTURE_RETRACES_PER_IMAGE == 0) {
            GXColor color = { (fixture_record[2] & 2) ? 192 : 32, 48, 96, 255 };
            GX_SetCopyClear(color, 0x00ffffff);
#if FIXTURE_TEXTURE
#if FIXTURE_TEXTURE_COPY
            GX_SetTexCopySrc(0, 0, 32, 32);
            GX_SetTexCopyDst(32, 32, GX_TF_RGB565, GX_FALSE);
            GX_CopyTex(texels, GX_FALSE);
            GX_PixModeSync();
#else
            for (u32 i = 0; i < 32 * 32; ++i)
                ((u16 *)texels)[i] = (fixture_record[2] & (FIXTURE_DEPTH ? 4 : 2)) ? 0x07e0 : 0x001f;
            DCFlushRange(texels, 32 * 32 * 2);
#endif
#if FIXTURE_DEPTH
            GX_InvalidateTexAll();
#endif
            GX_LoadTexObj(&texture, GX_TEXMAP0);
#if FIXTURE_DEPTH
            const f32 shift = (fixture_record[2] & 2) ? 0.0f : 0.2f;
            const f32 depth = (fixture_record[2] & 2) ? -0.25f : -0.75f;
            GX_Begin(GX_TRIANGLES, GX_VTXFMT0, 3);
            GX_Position3f32(-1 + shift, -1, depth); GX_TexCoord2f32(0, 0);
            GX_Position3f32(1 + shift, -1, depth); GX_TexCoord2f32(1, 0);
            GX_Position3f32(shift, 1, depth); GX_TexCoord2f32(0.5f, 1);
#else
            GX_Begin(GX_TRIANGLES, GX_VTXFMT0, 3);
            GX_Position3f32(-1, -1, 0); GX_TexCoord2f32(0, 0);
            GX_Position3f32(1, -1, 0); GX_TexCoord2f32(1, 0);
            GX_Position3f32(0, 1, 0); GX_TexCoord2f32(0.5f, 1);
#endif
            GX_End();
#endif
            GX_CopyDisp((void *)xfb, FIXTURE_CLEAR_EFB ? GX_TRUE : GX_FALSE);
#if FIXTURE_GX_PARTS == 2
            GX_CopyDisp((u8 *)xfb + mode->fbWidth * mode->xfbHeight, GX_TRUE);
#endif
            GX_DrawDone();
        }
#else
        xfb[0] = 0x10801080 ^ (((fixture_record[2] / FIXTURE_RETRACES_PER_IMAGE) & 1) << 24);
#endif
        fixture_observe();
    }
}
