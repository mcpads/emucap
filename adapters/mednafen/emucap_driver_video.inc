// Included by native main.cpp after the driver video globals.
static EmucapCompletedFrame EmucapPublishedFrame;

static std::array<EmucapDriverFrames::View, 2> EmucapDriverViews() noexcept
{
 return {{{SoftFB[0].surface.get(), &SoftFB[0].rect, SoftFB[0].lw.get(), &SoftFB[0].field},
          {SoftFB[1].surface.get(), &SoftFB[1].rect, SoftFB[1].lw.get(), &SoftFB[1].field}}};
}

EmucapDriverVideo::EmucapDriverVideo() : mutex_(VTMutex)
{
 if(!mutex_ || !MThreading::Mutex_Lock(static_cast<MThreading::Mutex*>(mutex_)))
  throw std::runtime_error("driver video lock unavailable");
}

EmucapDriverVideo::~EmucapDriverVideo() noexcept
{
 if(!MThreading::Mutex_Unlock(static_cast<MThreading::Mutex*>(mutex_)))
  std::terminate();
}

EmucapDriverFrames EmucapDriverVideo::capture() const
{
 return EmucapDriverFrames::capture(EmucapDriverViews(), SoftFB_BackBuffer);
}

bool EmucapDriverVideo::can_restore(const EmucapDriverFrames& frames) const noexcept
{
 return frames.can_swap_into(EmucapDriverViews(), SoftFB_BackBuffer);
}

bool EmucapDriverVideo::restore(EmucapDriverFrames& frames) noexcept
{
 return frames.swap_into(EmucapDriverViews(), SoftFB_BackBuffer);
}

bool EmucapDriverVideo::publish(EmucapCompletedFrame& image) noexcept
{
 if(!CurGame) return false;
 const bool queued_snapshot = VTReady.load(std::memory_order_acquire) >= 0 && VTSSnapshot;
 EmucapPublishedFrame.swap(image);
 VTRotated = CurGame->rotated;
 if(EmucapPublishedFrame.surface())
 {
  VTSSnapshot = queued_snapshot || pending_ssnapshot;
  pending_ssnapshot = false;
  VTReady.store(2, std::memory_order_release);
 }
 else
 {
  pending_ssnapshot = pending_ssnapshot || queued_snapshot;
  VTSSnapshot = false;
  VTReady.store(-1, std::memory_order_release);
 }
 // Native display also polls every millisecond; wakeup is a hint, not commit.
 if(VTWakeupSem) MThreading::Sem_Post(VTWakeupSem);
 return true;
}

// Called by MainVideoLoop while VTMutex is held.
static void EmucapBlitReadyFrame(int slot)
{
 if(slot == 2)
 {
  if(!EmucapPublishedFrame.surface())
   throw std::runtime_error("missing restored display frame");
  BlitScreen(EmucapPublishedFrame.surface(), &EmucapPublishedFrame.rect(),
             EmucapPublishedFrame.line_widths(), VTRotated, EmucapPublishedFrame.field(), VTSSnapshot);
 }
 else if(slot == 0 || slot == 1)
  BlitScreen(SoftFB[slot].surface.get(), &SoftFB[slot].rect, SoftFB[slot].lw.get(),
             VTRotated, SoftFB[slot].field, VTSSnapshot);
 else
  throw std::runtime_error("invalid display frame slot");
}
