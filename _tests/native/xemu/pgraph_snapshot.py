#!/usr/bin/env python3
"""Exercise the native PGRAPH codec: bit preservation, bounded prefixes and rejection.

The actual header is compiled against an in-memory QEMUFile transport. The fixture
models only fields touched by the codec; a native build verifies its actual types.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=ROOT/'adapters/xemu/work/xemu/hw/xbox/nv2a/pgraph/snapshot.h')
a = p.parse_args()
preamble = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <limits.h>
#define NV2A_MAX_BATCH_LENGTH 0x07ffff
#define NV2A_VERTEXSHADER_ATTRIBUTES 16
#define NV2A_MAX_TEXTURES 4
#define NV2A_MAX_LIGHTS 8
#define ARRAY_SIZE(a) (sizeof(a)/sizeof((a)[0]))
#define MIN(a,b) ((a)<(b)?(a):(b))
#define MAX(a,b) ((a)>(b)?(a):(b))
typedef struct {
    bool dma_select; uint64_t offset; unsigned inline_array_offset;
    float inline_value[4]; unsigned format,size,count; uint32_t stride;
    bool needs_conversion; float *inline_buffer; bool inline_buffer_populated;
} VertexAttribute;
typedef struct {
    unsigned inline_buffer_length,inline_array_length,inline_elements_length;
    unsigned draw_arrays_length,draw_arrays_min_start,draw_arrays_max_count;
    int32_t draw_arrays_start[1250],draw_arrays_count[1250];
    bool draw_arrays_prevent_connect;
    struct { uint64_t object_instance; uint32_t beta; } beta;
    bool zpass_pixel_count_enable,texture_matrix_enable[4];
    uint16_t compressed_attrs,uniform_attrs,swizzle_attrs;
    float material_alpha,light_infinite_half_vector[8][3],light_infinite_direction[8][3];
    float light_local_position[8][3],light_local_attenuation[8][3];
    float specular_params[6],specular_power,specular_params_back[6],specular_power_back,point_params[8];
    VertexAttribute vertex_attributes[16];
} PGRAPHState;
typedef struct { uint8_t *data; size_t length,position,capacity; int error; } QEMUFile;
static void qemu_put_byte(QEMUFile *f, unsigned value) {
    if (f->length==f->capacity) {f->capacity=f->capacity?f->capacity*2:2048;f->data=realloc(f->data,f->capacity);assert(f->data);}
    f->data[f->length++]=value;
}
static unsigned qemu_get_byte(QEMUFile *f) {
    if(f->position>=f->length) {f->error=-EIO;return 0;}
    return f->data[f->position++];
}
static void qemu_put_be16(QEMUFile *f,uint16_t v) {qemu_put_byte(f,v>>8);qemu_put_byte(f,v);}
static void qemu_put_be32(QEMUFile *f,uint32_t v) {qemu_put_be16(f,v>>16);qemu_put_be16(f,v);}
static void qemu_put_be64(QEMUFile *f,uint64_t v) {qemu_put_be32(f,v>>32);qemu_put_be32(f,v);}
static uint16_t qemu_get_be16(QEMUFile *f) {unsigned a=qemu_get_byte(f);return a<<8|qemu_get_byte(f);}
static uint32_t qemu_get_be32(QEMUFile *f) {uint32_t a=qemu_get_be16(f);return a<<16|qemu_get_be16(f);}
static uint64_t qemu_get_be64(QEMUFile *f) {uint64_t a=qemu_get_be32(f);return a<<32|qemu_get_be32(f);}
static int qemu_file_get_error(QEMUFile *f) {return f->error;}
static unsigned allocation_attempts,fail_allocation,live_allocations;
static void *g_try_malloc(size_t n) {
    if (++allocation_attempts==fail_allocation) return NULL;
    void *p=malloc(n);if(p)live_allocations++;return p;
}
static void *g_try_malloc0(size_t n) {void *p=g_try_malloc(n);if(p)memset(p,0,n);return p;}
static void g_free(void *p) {if(p){assert(live_allocations);live_allocations--;free(p);}}
'''
cases = r'''
static void rewind_file(QEMUFile *f) {f->position=0;f->error=0;}
static void reject_unchanged(QEMUFile *f,PGRAPHState *target) {
    PGRAPHState before=*target;rewind_file(f);
    assert(pgraph_snapshot_get(f,target)<0);
    assert(!memcmp(target,&before,sizeof(before)) && !live_allocations);
}
int main(void) {
    PGRAPHState source={0},target={0};
    source.draw_arrays_min_start=UINT32_MAX;
    source.beta.object_instance=0x1020304050607080ULL;source.beta.beta=0xaabbccdd;
    source.texture_matrix_enable[3]=true;source.zpass_pixel_count_enable=true;
    source.compressed_attrs=0x8001;source.uniform_attrs=0x4422;source.swizzle_attrs=0x1234;
    uint32_t patterns[]={0x80000000,0x7fc01234,0xff800000,0x3f800000};
    memcpy(&source.material_alpha,patterns,4);
    float *extra[]={source.light_infinite_half_vector[0],source.light_infinite_direction[0],
        source.light_local_position[0],source.light_local_attenuation[0],source.specular_params,
        &source.specular_power,source.specular_params_back,&source.specular_power_back,source.point_params};
    unsigned sizes[]={24,24,24,24,6,1,6,1,8};
    for(unsigned k=0;k<ARRAY_SIZE(extra);k++)for(unsigned j=0;j<sizes[k];j++)memcpy(extra[k]+j,&patterns[j%4],4);
    for(unsigned i=0;i<16;i++) {
        VertexAttribute *v=&source.vertex_attributes[i];
        v->dma_select=i%2;v->offset=0x12340+i*16;v->format=2;v->size=4;v->count=i%5;v->stride=36;
        memcpy(v->inline_value,patterns,16);
    }
    /* Unpopulated/empty storage is not read, even with a null pointer. */
    source.vertex_attributes[0].inline_buffer_populated=true;
    QEMUFile empty={0};assert(!pgraph_snapshot_put(&empty,&source));
    assert(empty.data[3]==1 && !empty.data[4]);
    assert(!memcmp(empty.data+13,"\x10\x20\x30\x40\x50\x60\x70\x80",8));
    assert(!memcmp(empty.data+36,"\x80\0\0\0",4));
    assert(!pgraph_snapshot_get(&empty,&target));
    QEMUFile again={0};assert(!pgraph_snapshot_put(&again,&target));
    assert(empty.length==again.length&&!memcmp(empty.data,again.data,empty.length));free(again.data);
    /* Every byte truncation rejects without publishing any command-state field. */
    size_t length=empty.length;
    for(size_t n=0;n<length;n++){empty.length=n;reject_unchanged(&empty,&target);}empty.length=length;
    size_t invalid_offsets[]={3,12,25,26,512,524,532,534};
    unsigned invalid_values[]={2,2,2,2,2,3,5,1};
    for(unsigned i=0;i<ARRAY_SIZE(invalid_offsets);i++) {
        size_t at=invalid_offsets[i];uint8_t before=empty.data[at];empty.data[at]=invalid_values[i];
        reject_unchanged(&empty,&target);empty.data[at]=before;
    }
    allocation_attempts=0;fail_allocation=1;reject_unchanged(&empty,&target);fail_allocation=0;
    target.inline_buffer_length=NV2A_MAX_BATCH_LENGTH+1;reject_unchanged(&empty,&target);target.inline_buffer_length=0;
    target.inline_elements_length=NV2A_MAX_BATCH_LENGTH+1;reject_unchanged(&empty,&target);target.inline_elements_length=0;
    target.draw_arrays_length=1251;reject_unchanged(&empty,&target);target.draw_arrays_length=0;

    /* Pending draws retain the minimum and connectivity across a different future. */
    source.draw_arrays_length=target.draw_arrays_length=1250;
    for(unsigned i=0;i<1250;i++) {
        source.draw_arrays_start[i]=target.draw_arrays_start[i]=i*2;
        source.draw_arrays_count[i]=target.draw_arrays_count[i]=2;
    }
    source.draw_arrays_max_count=target.draw_arrays_max_count=2500;
    source.draw_arrays_min_start=0;source.draw_arrays_prevent_connect=true;
    target.draw_arrays_min_start=123;target.draw_arrays_prevent_connect=false;
    QEMUFile draws={0};assert(!pgraph_snapshot_put(&draws,&source));
    assert(!pgraph_snapshot_get(&draws,&target));
    assert(target.draw_arrays_min_start==0&&target.draw_arrays_prevent_connect);
    target.draw_arrays_count[0]=-1;reject_unchanged(&draws,&target);target.draw_arrays_count[0]=2;
    target.draw_arrays_max_count=2499;reject_unchanged(&draws,&target);target.draw_arrays_max_count=2500;
    source.draw_arrays_min_start=1;QEMUFile invalid={0};assert(pgraph_snapshot_put(&invalid,&source)<0&&!invalid.length);
    source.draw_arrays_length=target.draw_arrays_length=0;source.draw_arrays_min_start=UINT32_MAX;
    free(draws.data);
    float all_data[16][8],all_destination[16][8];
    source.inline_buffer_length=target.inline_buffer_length=2;
    for(unsigned i=0;i<16;i++) {
        for(unsigned j=0;j<8;j++)all_data[i][j]=(float)(i*100+j);
        source.vertex_attributes[i].inline_buffer_populated=true;
        source.vertex_attributes[i].inline_buffer=all_data[i];
        target.vertex_attributes[i].inline_buffer=all_destination[i];
    }
    QEMUFile all={0};assert(!pgraph_snapshot_put(&all,&source));
    memset(all_destination,0xcc,sizeof(all_destination));assert(!pgraph_snapshot_get(&all,&target));
    assert(!memcmp(all_data,all_destination,sizeof(all_data))&&!live_allocations);free(all.data);
    for(unsigned i=0;i<16;i++) {
        source.vertex_attributes[i].inline_buffer_populated=(i==0);
        source.vertex_attributes[i].inline_buffer=target.vertex_attributes[i].inline_buffer=NULL;
    }
    unsigned counts[]={1,NV2A_MAX_BATCH_LENGTH};
    for(unsigned trial=0;trial<2;trial++) {
        unsigned count=counts[trial];size_t bytes=(size_t)count*16;
        float *data=malloc(bytes+16),*destination=malloc(bytes+16);assert(data&&destination);
        for(size_t i=0;i<bytes/4;i++)memcpy(data+i,&patterns[i%4],4);
        memset((uint8_t*)data+bytes,0xcc,16);memset(destination,0xee,bytes+16);
        source.inline_buffer_length=target.inline_buffer_length=count;
        source.vertex_attributes[0].inline_buffer=data;target.vertex_attributes[0].inline_buffer=destination;
        QEMUFile f={0};assert(!pgraph_snapshot_put(&f,&source));assert(f.length==length+bytes);
        if(!trial){
            allocation_attempts=0;fail_allocation=2;reject_unchanged(&f,&target);fail_allocation=0;
            f.length--;reject_unchanged(&f,&target);f.length++;
            for(size_t j=0;j<bytes+16;j++)assert(((uint8_t*)destination)[j]==0xee);
        }
        rewind_file(&f);assert(!pgraph_snapshot_get(&f,&target));assert(!live_allocations);
        assert(target.vertex_attributes[0].inline_buffer==destination&&!memcmp(data,destination,bytes));
        for(unsigned j=0;j<16;j++)assert(((uint8_t*)destination)[bytes+j]==0xee);
        QEMUFile second={0};memset((uint8_t*)data+bytes,0xaa,16);assert(!pgraph_snapshot_put(&second,&source));
        assert(f.length==second.length&&!memcmp(f.data,second.data,f.length));
        free(second.data);free(f.data);free(data);free(destination);
    }
    free(empty.data);return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-pgraph-snapshot-') as directory:
    d=Path(directory)
    (d/'test.c').write_text(preamble+'\n'+a.source.read_text()+'\n'+cases)
    subprocess.run(['cc','-std=c11','-O1','-g','-fsanitize=address,undefined','-Wall','-Wextra','-Werror','-Wno-sign-compare',str(d/'test.c'),'-o',str(d/'test')],check=True)
    subprocess.run([str(d/'test')],check=True)
print('PGRAPH bits, active prefixes, poisoned capacity, truncation, malformed fields and allocation failures passed')
