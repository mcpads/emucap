#!/usr/bin/env python3
"""Verify one leaf barrier covers native and external texture upload callers without dropping calls."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
source = (ROOT / 'adapters/xemu/work/xemu/ui/xemu-gl-upload.c').read_text()
source = '\n'.join(line for line in source.splitlines() if not line.startswith('#include'))
preamble = r'''
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>
#define G_LOCK_DEFINE_STATIC(name) static pthread_mutex_t lock_##name = PTHREAD_MUTEX_INITIALIZER
#define G_LOCK(name) assert(pthread_mutex_lock(&lock_##name) == 0)
#define G_UNLOCK(name) assert(pthread_mutex_unlock(&lock_##name) == 0)
static atomic_int active, calls;
static int pixels;
static void glTexImage2D(unsigned target, int level, int format, int width,
                        int height, int border, unsigned external, unsigned type,
                        const void *data) {
    assert(target==1 && level==2 && format==3 && width==4 && height==5);
    assert(border==6 && external==7 && type==8 && data==&pixels);
    assert(atomic_fetch_add(&active,1)==0);
    usleep(1000);
    assert(atomic_fetch_sub(&active,1)==1);
    atomic_fetch_add(&calls,1);
}
'''
cases = r'''
static void *upload(void *opaque) {
    (void)opaque;
    for(int i=0;i<64;i++) {
        if(i%2) {
            xemu_gl_upload_begin();
            glTexImage2D(1,2,3,4,5,6,7,8,&pixels);
            xemu_gl_upload_end();
        } else {
            xemu_gl_tex_image_2d(1,2,3,4,5,6,7,8,&pixels);
        }
    }
    return NULL;
}
int main(void) {
    pthread_t threads[8];
    for(int i=0;i<8;i++) assert(pthread_create(&threads[i],NULL,upload,NULL)==0);
    for(int i=0;i<8;i++) assert(pthread_join(threads[i],NULL)==0);
    assert(calls==512 && active==0);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-gl-upload-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(preamble + source + cases)
    subprocess.run(['cc','-D__APPLE__','-std=c11','-D_DEFAULT_SOURCE','-O2','-Wall','-Wextra',
                    '-Werror','-pthread',str(temp/'test.c'),'-o',str(temp/'test')],check=True)
    subprocess.run([str(temp/'test')],check=True)
print('512 uploads: preserved arguments and shared cross-thread exclusion')
