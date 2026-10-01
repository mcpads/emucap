#!/usr/bin/env python3
"""Native interrupt sequence and snapshot-record admission regressions."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True, help='patched core src directory')
a = p.parse_args()
source = r'''
#include "device/r4300/interrupt_jitter.h"
#include <assert.h>
#include <stdio.h>
int main(void) {
    const uint32_t vectors[] = {270369,67634689,2647435461,307599695,2398689233};
    uint32_t state=1,other=777;
    for (unsigned i=0;i<5;++i) {
        unsigned result=interrupt_jitter_next(&state,1);
        assert(state==vectors[i] && result==(vectors[i]&63));
        interrupt_jitter_next(&other,1);
    }
    uint32_t unchanged=state;
    assert(interrupt_jitter_next(&state,0)==0 && state==unchanged);
    unsigned char record[32],invalid[32];
    assert(interrupt_jitter_encode(record,32,state));
    assert(record[8]==1 && record[9]==0 && record[12]==(state&255));
    uint32_t sequence[100];
    for(unsigned i=0;i<100;++i)sequence[i]=interrupt_jitter_next(&state,1);
    assert(interrupt_jitter_decode(record,32,&state)==1 && state==unchanged);
    for(unsigned i=0;i<100;++i)assert(interrupt_jitter_next(&state,1)==sequence[i]);
    unchanged=state;
    for(unsigned size=0;size<32;++size) {
        assert(interrupt_jitter_decode(record,size,&state)==-1 && state==unchanged);
    }
    // Every one-bit corruption is rejected, including marker, encoding and reserved bytes.
    for(unsigned byte=0;byte<32;++byte)for(unsigned bit=0;bit<8;++bit){
        memcpy(invalid,record,32);invalid[byte]^=1u<<bit;
        assert(interrupt_jitter_decode(invalid,32,&state)==-1 && state==unchanged);
    }
    memcpy(invalid,record,32);
    interrupt_jitter_write32(invalid+12,0);interrupt_jitter_write32(invalid+16,UINT32_MAX);
    assert(interrupt_jitter_decode(invalid,32,&state)==-1 && state==unchanged);
    memset(invalid,0,32);assert(interrupt_jitter_decode(invalid,32,&state)==0 && state==unchanged);
    memset(invalid,0xa5,32);unsigned char before[32];memcpy(before,invalid,32);
    assert(!interrupt_jitter_encode(invalid,32,0) && !memcmp(before,invalid,32));
    puts("native jitter vectors, instance isolation, continuation, legacy and malformed records passed");
}
'''
with tempfile.TemporaryDirectory(prefix='n64-jitter-') as d:
    f=Path(d)/'test.c';f.write_text(source)
    subprocess.run(['cc','-std=c99','-Wall','-Wextra','-Werror','-fsanitize=address,undefined','-fno-sanitize-recover=all','-I',str(a.source),str(f),'-o',d+'/test'],check=True)
    subprocess.run([d+'/test'],check=True)
