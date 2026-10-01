#!/usr/bin/env python3
"""Exercise native load preparation, immutable ownership, replacement and cleanup."""
import argparse
import gzip
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args();s=(a.source/'main/savestates.c').read_text()
def body(sig):
    start=s.index(sig);i=s.index('{',start)+1;depth=1
    while depth:
        depth+=(s[i]=='{')-(s[i]=='}');i+=1
    return s[start:i]
struct=s[s.index('struct prepared_native_state {'):s.index('/* Returns the malloc\'d full path')]
pre=r'''
#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zlib.h>
#include "device/r4300/interrupt_jitter.h"
typedef enum {savestates_job_nothing,savestates_job_load,savestates_job_save} savestates_job;
typedef enum {savestates_type_unknown,savestates_type_m64p,savestates_type_pj64_zip,savestates_type_pj64_unc} savestates_type;
enum {M64ERR_SUCCESS,M64ERR_UNSUPPORTED,M64ERR_INVALID_STATE,M64ERR_INPUT_INVALID,M64ERR_NO_MEMORY};
#define M64MSG_STATUS 0
#define OSD_BOTTOM_LEFT 0
#define M64CORE_STATE_LOADCOMPLETE 1
#define main_message(...) ((void)0)
#define DebugMessage(...) ((void)0)
static void *savestates_lock;
void SDL_LockMutex(void *x){}
void SDL_UnlockMutex(void *x){}
void SDL_DestroyMutex(void *x){}
static const unsigned savestate_latest_version=0x10900;
static const char savestate_magic[]="M64+SAVE",pj64_magic[]={0xC8,0xA6,0xD8,0x23};
static struct {char MD5[32];} ROM_SETTINGS;
static savestates_job job;
static savestates_type type;
static char *fname;
static unsigned opened,applied,completed;
static FILE *osal_file_open(const char *p,const char *m){++opened;return fopen(p,m);}
static gzFile osal_gzopen(const char *p,const char *m){++opened;return gzopen(p,m);}
static void StateChanged(int kind,int result){assert(kind==M64CORE_STATE_LOADCOMPLETE && result==1);++completed;}
struct device {int unused;} g_dev;
'''
code=pre+struct+body('void savestates_set_job(')+body('static void savestates_clear_job(')+body('static struct prepared_native_state *read_native_state(')+body('static savestates_type savestates_detect_type(')+body('int savestates_prepare_load(')
code+=r'''
// The real dispatch must hand the decoder the admitted image without reopening its name.
static int savestates_load_m64p(struct device *dev,char *path){
 assert(prepared_load && prepared_load->data[0]==0x42 && prepared_load->has_jitter);
 ++applied;discard_prepared_load();return 1;
}
static char *savestates_generate_path(savestates_type t){return fname?strdup(fname):NULL;}
static int savestates_load_pj64_zip(struct device*d,char*p){abort();}
static int savestates_load_pj64_unc(struct device*d,char*p){abort();}
'''+body('int savestates_load(')+body('void savestates_deinit(')+r'''
int main(int argc,char **argv){
 assert(argc==6);memset(ROM_SETTINGS.MD5,'A',32);
 for(int i=2;i<5;i++){
  assert(savestates_prepare_load(argv[i])==M64ERR_INPUT_INVALID);
  assert(!prepared_load && job==savestates_job_nothing && applied==0);
 }
 assert(savestates_prepare_load(argv[1])==M64ERR_SUCCESS);
 void *admitted=prepared_load;
 assert(savestates_prepare_load(argv[2])==M64ERR_INVALID_STATE && prepared_load==admitted);
 assert(savestates_prepare_load(NULL)==M64ERR_INVALID_STATE && prepared_load==admitted);
 assert(unlink(argv[1])==0);unsigned before=opened;
 assert(savestates_load()==1 && opened==before && applied==1 && completed==1);
 assert(!prepared_load && !fname && job==savestates_job_nothing);
 // A legacy image still prepares, without inventing a restored generator record.
 assert(savestates_prepare_load(argv[5])==M64ERR_SUCCESS && !prepared_load->has_jitter);
 savestates_set_job(savestates_job_nothing,savestates_type_unknown,NULL);
 assert(!prepared_load && !fname && job==savestates_job_nothing);
 assert(savestates_prepare_load(argv[5])==M64ERR_SUCCESS);
 savestates_set_job(savestates_job_save,savestates_type_m64p,argv[5]);
 assert(!prepared_load && job==savestates_job_save);
 savestates_clear_job();
 assert(savestates_prepare_load(argv[5])==M64ERR_SUCCESS);
 savestates_deinit();
 assert(!prepared_load && !fname && job==savestates_job_nothing);
 assert(savestates_prepare_load(NULL)==M64ERR_UNSUPPORTED);
 puts("native preparation: rejection, immutable consumption, duplicate refusal and cleanup passed");
}
'''
with tempfile.TemporaryDirectory(prefix='n64-prepared-') as d:
    d=Path(d);f=d/'test.c';f.write_text(code)
    good=bytearray(16793412);good[:8]=b'M64+SAVE';good[8:12]=(0x10900).to_bytes(4,'big');good[12:44]=b'A'*32;good[44]=0x42
    good[-32:-24]=b'ECJITTER';good[-24:-20]=(1).to_bytes(4,'little');good[-20:-16]=(7).to_bytes(4,'little');good[-16:-12]=((~7)&0xffffffff).to_bytes(4,'little')
    bad=bytearray(good);bad[-24]=2
    wrong_rom=bytearray(good);wrong_rom[12]=ord('B')
    legacy=bytearray(good);legacy[-32:]=bytes(32)
    paths=[]
    for name,data in [('good',good),('bad',bad),('truncated',good[:-1]),('wrong-rom',wrong_rom),('legacy',legacy)]:
        path=d/(name+'.gz');path.write_bytes(gzip.compress(data));paths.append(str(path))
    subprocess.run(['cc','-std=c99','-fsanitize=address,undefined','-fno-sanitize-recover=all','-I',str(a.source),str(f),'-lz','-o',str(d/'test')],check=True)
    subprocess.run([str(d/'test'),*paths],check=True)
