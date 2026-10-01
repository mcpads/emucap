/* Standalone diagnostic for concurrent Apple GL pixel conversion.
 * Build/run through macos_gl_upload.py, which links the native upload wrapper.
 * A passing run bounds this probe; it does not establish the emulator crash cause.
 */
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern void xemu_gl_tex_image_2d(unsigned int,int,int,int,int,int,unsigned int,unsigned int,const void *);
static GLuint texture_names[2];
static int shared;
static pthread_mutex_t start_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t start_cond = PTHREAD_COND_INITIALIZER;
static int ready, serialize, iterations;
static CGLContextObj contexts[2];

static void *worker(void *opaque) {
    intptr_t index = (intptr_t)opaque;
    assert(CGLSetCurrentContext(contexts[index]) == kCGLNoError);
    if (!index) printf("renderer=%s version=%s\n", glGetString(GL_RENDERER), glGetString(GL_VERSION));
    GLuint texture;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    size_t count = 1280 * 720;
    uint8_t *rgba = malloc(count*4), *readback = malloc(count*4);
    uint16_t *rgb565 = malloc(count*2), *bgra5551 = malloc(count*2);
    assert(rgba && readback && rgb565 && bgra5551);
    for (size_t i=0;i<count;i++) {
        rgba[4*i]=0; rgba[4*i+1]=0; rgba[4*i+2]=255; rgba[4*i+3]=255;
        rgb565[i]=0xf800; bgra5551[i]=0xfc00;
    }
    pthread_mutex_lock(&start_lock);
    texture_names[index] = texture;
    ready++;
    pthread_cond_broadcast(&start_cond);
    while (ready<2) pthread_cond_wait(&start_cond,&start_lock);
    if (shared) assert(texture_names[0] != texture_names[1]);
    printf("thread=%ld texture=%u\n", (long)index, texture);
    fflush(stdout);
    pthread_mutex_unlock(&start_lock);
    for (int i=0;i<iterations;i++) {
        int width = i%2 ? 640 : 1280, height = i%2 ? 480 : 720;
        GLenum internal = GL_RGB, format = GL_BGRA, type = GL_UNSIGNED_BYTE;
        const void *pixels = rgba;
        if (index && i%3==0) { internal=GL_RGB5_A1; type=GL_UNSIGNED_SHORT_1_5_5_5_REV; pixels=bgra5551; }
        if (index && i%3==1) { internal=GL_RGB565; format=GL_RGB; type=GL_UNSIGNED_SHORT_5_6_5; pixels=rgb565; }
        if (serialize)
            xemu_gl_tex_image_2d(GL_TEXTURE_2D,0,internal,width,height,0,format,type,pixels);
        else
            glTexImage2D(GL_TEXTURE_2D,0,internal,width,height,0,format,type,pixels);
        assert(glGetError()==GL_NO_ERROR);
        if (i%64==0 || i==iterations-1) {
            glGetTexImage(GL_TEXTURE_2D,0,GL_RGBA,GL_UNSIGNED_BYTE,readback);
            assert(glGetError()==GL_NO_ERROR);
            for (int p=0;p<width*height;p++)
                assert(readback[4*p]==255 && readback[4*p+1]==0 && readback[4*p+2]==0 && readback[4*p+3]==255);
        }
    }
    glFinish();
    glDeleteTextures(1,&texture);
    free(rgba);free(readback);free(rgb565);free(bgra5551);
    assert(CGLSetCurrentContext(NULL)==kCGLNoError);
    return NULL;
}

int main(int argc,char **argv) {
    assert(argc==4);
    serialize=atoi(argv[1]); int share=atoi(argv[2]); shared=share; iterations=atoi(argv[3]);
    assert((serialize==0 || serialize==1) && (share==0 || share==1) && iterations>0);
    CGLPixelFormatAttribute attrs[]={kCGLPFAAccelerated,kCGLPFAOpenGLProfile,(CGLPixelFormatAttribute)kCGLOGLPVersion_3_2_Core,0};
    CGLPixelFormatObj format; GLint count;
    assert(CGLChoosePixelFormat(attrs,&format,&count)==kCGLNoError && count>0);
    assert(CGLCreateContext(format,NULL,&contexts[0])==kCGLNoError);
    assert(CGLCreateContext(format,share?contexts[0]:NULL,&contexts[1])==kCGLNoError);
    CGLDestroyPixelFormat(format);
    pthread_t threads[2];
    for (intptr_t i=0;i<2;i++) assert(pthread_create(&threads[i],NULL,worker,(void*)i)==0);
    for (int i=0;i<2;i++) assert(pthread_join(threads[i],NULL)==0);
    CGLDestroyContext(contexts[1]); CGLDestroyContext(contexts[0]);
    printf("passed serialize=%d shared=%d uploads=%d\n",serialize,share,iterations*2);
    return 0;
}
