#include "emucap_native_control.h"
#include <cassert>
using EmucapControl::NativeControl;
int main() {
  NativeControl state;
  assert(!state.Parked(true));
  {
    NativeControl::Scope frame(state, NativeControl::Context::Frame);
    assert(state.Parked(true) && !state.Parked(false));
    const auto origin = state.Generation();
    state.GenerationChanged();
    assert(state.Generation() != origin && state.Parked(true));
    state.FrameCompleted();
    assert(state.Frames() == 1 && state.Callbacks() == 0);
    {
      NativeControl::Scope cpu(state, NativeControl::Context::Cpu);
      state.CpuCallback();
      assert(state.Parked(true));
      {
        NativeControl::Scope device(state, NativeControl::Context::Device);
        assert(!state.Parked(true));
      }
      assert(state.Parked(true));
      {
        NativeControl::Scope pacing(state, NativeControl::Context::Pacing);
        assert(!state.Parked(true));
        state.GenerationChanged();
      }
      // Returning from a nested hook must not certify the old CPU stack.
      assert(!state.Parked(true));
      {
        NativeControl::Scope fresh(state, NativeControl::Context::Cpu);
        state.CpuCallback();
        assert(state.Parked(true));
      }
      assert(!state.Parked(true));
    }
    assert(state.Parked(true));
    {
      NativeControl::Scope device(state, NativeControl::Context::Device);
      assert(!state.Parked(true));
    }
    assert(state.Parked(true));
    {
      const auto callbacks = state.Callbacks();
      NativeControl::Scope idle(state, NativeControl::Context::Idle);
      assert(!state.Parked(true) && state.Callbacks() == callbacks);
    }
    assert(state.Parked(true));
  }
  assert(state.Valid() && !state.Parked(true));
  assert(state.Frames() == 1 && state.Callbacks() == 2);
  {
    const auto callbacks = state.Callbacks();
    NativeControl::Scope restored(state, NativeControl::Context::Continuation);
    assert(state.Parked(true) && !state.Parked(false));
    assert(state.Callbacks() == callbacks);
    state.GenerationChanged();
    assert(!state.Parked(true));
    {
      NativeControl::Scope fresh(state, NativeControl::Context::Continuation);
      assert(state.Parked(true) && state.Callbacks() == callbacks);
    }
    assert(!state.Parked(true));
  }
  state.Invalidate();
  state.GenerationChanged();
  {
    NativeControl::Scope frame(state, NativeControl::Context::Frame);
    assert(!state.Valid() && !state.Parked(true));
    NativeControl::Scope cpu(state, NativeControl::Context::Cpu);
    assert(!state.Valid() && !state.Parked(true));
    NativeControl::Scope continuation(state, NativeControl::Context::Continuation);
    assert(!state.Valid() && !state.Parked(true));
  }
}
