#!/usr/bin/env python3
"""Exercise the applied texture rehash admission and secondary-cache selection.

Extract the production branches rather than reimplementing their decisions. GPU
objects and content hashes are symbolic here; renderer runtime proof is separate.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.read_text()
    admission = source[source.index("bool rehash = (entry->status"):source.index("\n\t\t// Do we need to recreate?")]
    check = source[source.index("bool TextureCacheCommon::CheckFullHash("):source.index("\n// One type of texture update")]
    support = r'''
#include <cassert>
#include <cstdint>
#include <map>
#include <memory>
using u32 = uint32_t;
using u64 = uint64_t;
using GETextureFormat = int;
#define PROFILE_THIS_SCOPE(x)
namespace TexStatus { enum { RELIABLE=1, HASH_RECHECK=2, CLUT_RECHECK=4 }; }
struct TexCacheEntry {
 int status=0, lastSyncDomain=-1, bufw=512, format=5, maxLevel=0, numInvalidated=4;
 u32 addr=0x1000, fullhash=0, cluthash=7;
 int dim=9;
 void *texturePtr=nullptr;
 bool MatchesProperties(int d, int f, int m) const { return d==dim && f==format && m==maxLevel; }
};
struct State {
 int getTextureWidth(int) { return 512; }
 int getTextureHeight(int) { return 32; }
 bool isTextureSwizzled() { return false; }
} gstate;
struct { int textureSyncTimeDomain=100; } gstate_c;
static u32 memoryHash=1;
static int hashCalls=0;
static u32 QuickTexHash(int, u32, int, int, int, bool, int, TexCacheEntry *) {
 ++hashCalls; return memoryHash;
}
using TexCache = std::map<u64, std::unique_ptr<TexCacheEntry>>;
class TextureCacheCommon {
public:
 int replacer_=0;
 bool lowMemoryMode_=false;
 TexCache secondCache_;
 TexCacheEntry *nextTexture_=nullptr;
 int released=0;
 bool IsVideo(u32) { return false; }
 void ReleaseTexture(TexCacheEntry *, bool) { ++released; }
 bool CheckFullHash(TexCacheEntry *, bool &);
};
'''
    wrapper = "\nstatic bool NeedsRehash(TexCacheEntry *entry) {\n" + admission + "\nreturn rehash;\n}\n"
    checks = r'''
static u64 Key(u32 hash, u32 clut) { return hash | ((u64)clut << 32); }
static TexCacheEntry *Select(TextureCacheCommon &cache, TexCacheEntry &primary) {
 cache.nextTexture_=nullptr;
 if (!NeedsRehash(&primary)) return &primary;
 bool doDelete=true;
 if (!cache.CheckFullHash(&primary,doDelete)) {
  primary.texturePtr=(void *)(uintptr_t)memoryHash; // fake native rebuild
  return &primary;
 }
 return cache.nextTexture_ ? cache.nextTexture_ : &primary;
}
int main() {
 // RAM A -> B -> C -> A, then repeated glyph draws in one sync domain.
 // A lives in the secondary cache while the primary still holds C.
 TextureCacheCommon cache;
 TexCacheEntry primary;
 primary.fullhash=3; primary.texturePtr=(void *)3;
 auto archived=std::make_unique<TexCacheEntry>();
 archived->fullhash=1; archived->texturePtr=(void *)1;
 TexCacheEntry *a=archived.get();
 cache.secondCache_[Key(1,7)]=std::move(archived);
 memoryHash=1;
 for (int i=0;i<8;++i) {
  auto chosen=Select(cache,primary);
  assert(chosen==a && chosen->fullhash==memoryHash && chosen->texturePtr==(void *)1);
  assert(primary.fullhash==3 && primary.texturePtr==(void *)3);
 }
 assert(cache.released==0);
 // Return to the primary's actual content: validate once, then keep the fast path.
 ++gstate_c.textureSyncTimeDomain; memoryHash=3;
 assert(Select(cache,primary)==&primary);
 const int calls=hashCalls;
 for (int i=0;i<8;++i) assert(Select(cache,primary)==&primary);
 assert(hashCalls==calls);
 // Unknown content rebuilds normally; an old matching hash with incompatible
 // dimensions must not select an incompatible archived GPU object.
 ++gstate_c.textureSyncTimeDomain; memoryHash=5;
 auto mismatch=std::make_unique<TexCacheEntry>();mismatch->dim=123;
 mismatch->fullhash=5;mismatch->texturePtr=(void *)99;
 cache.secondCache_[Key(5,7)]=std::move(mismatch);
 assert(Select(cache,primary)==&primary && primary.fullhash==5 && primary.texturePtr==(void *)5);
 // Empty cache and low-memory paths cannot bind an archived object.
 TextureCacheCommon empty;
 ++gstate_c.textureSyncTimeDomain; memoryHash=6;
 assert(Select(empty,primary)==&primary && primary.fullhash==6);
 cache.lowMemoryMode_=true;
 ++gstate_c.textureSyncTimeDomain; memoryHash=1;
 assert(Select(cache,primary)==&primary && primary.fullhash==1);
 // Explicit recheck wins even within the current sync domain.
 primary.status|=TexStatus::HASH_RECHECK;
 memoryHash=2;assert(Select(empty,primary)==&primary && primary.fullhash==2);
}
'''
    with tempfile.TemporaryDirectory(prefix="ppsspp-texture-cache-") as directory:
        path = Path(directory)
        (path / "test.cpp").write_text(support + wrapper + check + checks)
        subprocess.run(["c++", "-std=c++17", "-fsanitize=address,undefined", str(path / "test.cpp"), "-o", str(path / "test")], check=True)
        subprocess.run([str(path / "test")], check=True)
    print("PPSSPP secondary-cache repeated draws, primary fast path, mismatch, miss, low-memory and explicit recheck passed")


if __name__ == "__main__":
    main()
