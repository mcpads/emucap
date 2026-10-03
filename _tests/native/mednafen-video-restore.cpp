// Included in the native filter-history harness, using actual filter owners and
// actual driver transaction methods. The mutex seam checks scope ownership.
#include "emucap_video_restore.h"
#include <atomic>
#include <new>
#include <cstdlib>
static int fail_after = -1;
void* operator new(std::size_t size) {
 if(fail_after == 0) {fail_after = -1; throw std::bad_alloc();}
 if(fail_after > 0) --fail_after;
 if(auto p = std::malloc(size ? size : 1)) return p;
 throw std::bad_alloc();
}
void operator delete(void* p) noexcept {std::free(p);}
namespace MThreading {
 struct Mutex {bool locked = false;};
 struct Sem {};
 bool Mutex_Lock(Mutex* m) noexcept {assert(!m->locked); m->locked = true; return true;}
 bool Mutex_Unlock(Mutex* m) noexcept {assert(m->locked); m->locked = false; return true;}
 bool Sem_Post(Sem*) noexcept {return true;}
}
static struct {
 std::unique_ptr<MDFN_Surface> surface;
 MDFN_Rect rect;
 std::unique_ptr<int32[]> lw;
 int field;
} SoftFB[2];
static bool SoftFB_BackBuffer;
static std::atomic<int> VTReady{-1};
static unsigned VTRotated;
static bool VTSSnapshot, pending_ssnapshot;
static MThreading::Mutex video_mutex;
static auto* VTMutex = &video_mutex;
static MThreading::Sem* VTWakeupSem = nullptr;
static MDFNGI* CurGame = &game;
void BlitScreen(const MDFN_Surface*, const MDFN_Rect*, const int32*, int, int, bool) {abort();}
#include "emucap_driver_video.inc"

static bool equal(const EmucapVideoBlocks& a, const EmucapVideoBlocks& b) {
 return a.raster == b.raster && a.filters == b.filters && a.completed == b.completed;
}
static void fill_driver(unsigned seed) {
 for(unsigned i = 0; i < 2; ++i) {
  auto& buffer = SoftFB[i];
  buffer.surface.reset(new MDFN_Surface(nullptr, 8, 8, 8,
      MDFN_PixelFormat(MDFN_PixelFormat::ABGR32_8888)));
  buffer.rect = {0, 0, 8, 8}; buffer.field = i;
  buffer.lw.reset(new int32[8]{-1, 8, 8, 8, 8, 8, 8, 8});
  for(unsigned n = 0; n < 64; ++n) buffer.surface->pixels[n] = seed + i * 100 + n;
 }
}
static void test_composed_video() {
 for(unsigned mode = 0; mode < 5; ++mode) for(unsigned blur = 0; blur < 3; ++blur)
  for(bool saved_back : {false, true}) for(bool live_back : {false, true})
   for(bool present : {false, true}) {
    initialize(mode, blur); CurGame = &game;
    const auto tag = MDFN_PixelFormat::ABGR32_8888;
    frame(tag, 0); frame(tag, 1);
    fill_driver(1000); SoftFB_BackBuffer = saved_back;
    EmucapCompletedFrame observation;
    if(present) observation.capture(*SoftFB[0].surface, SoftFB[0].rect, SoftFB[0].lw.get(), 1);
    EmucapVideoBlocks origin;
    {EmucapDriverVideo guard; origin = EmucapVideoRestore::capture(guard, observation);}
    std::vector<std::vector<uint32>> expected;
    for(unsigned n = 2; n < 6; ++n) expected.push_back(frame(tag, n));
    fill_driver(4000); SoftFB_BackBuffer = live_back;
    observation.capture(*SoftFB[1].surface, SoftFB[1].rect, SoftFB[1].lw.get(), 0);
    VTReady = 1; VTSSnapshot = true; pending_ssnapshot = false;
    EmucapDriverVideo guard;
    const auto destination = EmucapVideoRestore::capture(guard, observation);
    auto unchanged = [&] {
     assert(equal(EmucapVideoRestore::capture(guard, observation), destination));
     assert(VTReady == 1 && VTSSnapshot && !pending_ssnapshot);
    };
    for(unsigned block = 0; block < 3; ++block) {
     auto bad = origin;
     auto& bytes = block == 0 ? bad.raster : block == 1 ? bad.filters : bad.completed;
     bool rejected = false;
     // Corrupt the magic, including the final participant after two preparations.
     bytes[0] ^= 255;
     try {auto p = EmucapVideoRestore::prepare(bad, guard);}
     catch(const std::exception&) {rejected = true;}
     assert(rejected); unchanged();
    }
    {
     MDFN_Surface wrong(nullptr, 4, 4, 4, MDFN_PixelFormat(tag));
     for(unsigned i = 0; i < 16; ++i) wrong.pixels[i] = i;
     int32 widths[4] = {-1, 4, 4, 4};
     EmucapCompletedFrame image; image.capture(wrong, {0, 0, 4, 4}, widths);
     auto bad = origin; bad.completed = image.encode();
     bool rejected = false;
     try {auto p = EmucapVideoRestore::prepare(bad, guard);}
     catch(const std::exception&) {rejected = true;}
     assert(rejected); unchanged();
    }
    // Each C++ allocation can fail without installing a partially prepared owner.
    unsigned allocation_failures = 0;
    bool preparation_succeeded = false;
    for(int fault = 0; fault < 100; ++fault) {
     bool rejected = false; fail_after = fault;
     try {auto p = EmucapVideoRestore::prepare(origin, guard);}
     catch(const std::bad_alloc&) {rejected = true; ++allocation_failures;}
     fail_after = -1; unchanged();
     if(!rejected) {preparation_succeeded = true; break;}
    }
    assert(allocation_failures > 2 && preparation_succeeded);
    auto staged = EmucapVideoRestore::prepare(origin, guard); unchanged();
    SoftFB[1].field = 2; assert(!staged->commit(guard, observation)); SoftFB[1].field = 1;
    unchanged();
    deint_mode = (mode + 1) % 5; assert(!staged->commit(guard, observation)); deint_mode = mode;
    unchanged();
    // Publication rejection occurs after reversible native filter/raster swaps.
    CurGame = nullptr; assert(!staged->commit(guard, observation)); CurGame = &game;
    unchanged();
    auto* pixels0 = SoftFB[0].surface->pixels; auto* pixels1 = SoftFB[1].surface->pixels;
    fail_after = 0; assert(staged->commit(guard, observation)); assert(fail_after == 0); fail_after = -1;
    assert(equal(EmucapVideoRestore::capture(guard, observation), origin));
    assert(EmucapPublishedFrame.encode() == origin.completed);
    assert(VTReady == (present ? 2 : -1));
    assert(VTSSnapshot == present && pending_ssnapshot == !present);
    assert(pixels0 == SoftFB[0].surface->pixels && pixels1 == SoftFB[1].surface->pixels);
    assert(!staged->commit(guard, observation));
    assert(equal(EmucapVideoRestore::capture(guard, observation), origin));
    for(unsigned n = 2; n < 6; ++n) assert(frame(tag, n) == expected[n-2]);
   }
}
