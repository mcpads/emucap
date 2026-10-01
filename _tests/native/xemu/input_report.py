#!/usr/bin/env python3
"""Guest USB reports apply managed input independently of cached host polling."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source-root', type=Path, default=ROOT/'adapters/xemu/work/xemu')
a = p.parse_args()
def function(file, signature):
    source=(a.source_root/file).read_text();start=source.index(signature)
    return source[start:source.index('\n}',start)+2]
header=(a.source_root/'ui/xemu-input.h').read_text()
def enum(name):
    start=header.index('enum '+name+' {');return header[start:header.index('};',start)+2]
apply=function('ui/xemu-emucap.c','void xemu_emucap_apply_input(')
update=function('ui/xemu-input.c','void xemu_input_update_controller(')
report=function('hw/xbox/xid.c','void update_input(')
xid=(a.source_root/'hw/xbox/xid.h').read_text()
source=r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#define G_LOCK(x) do { assert(!held); held=true; } while(0)
#define G_UNLOCK(x) do { assert(held); held=false; } while(0)
#define QEMU_CLOCK_REALTIME 0
#define XEMU_INPUT_MIN_INPUT_UPDATE_INTERVAL_US 2500
#define ABS(x) llabs(x)
'''+enum('controller_state_buttons_mask')+'\n'+enum('controller_state_axis_index')+'\n'+enum('controller_input_device_type')+'\n'
source+='\n'.join(line for line in xid.splitlines() if line.startswith('#define GAMEPAD_') or line.startswith('#define BUTTON_MASK'))
source+=r'''
typedef struct { uint16_t buttons; int16_t axis[CONTROLLER_AXIS__COUNT];
                 int64_t last_input_updated_ts; int type; } ControllerState;
typedef struct { uint8_t bAnalogButtons[8]; uint16_t wButtons;
                 int16_t sThumbLX,sThumbLY,sThumbRX,sThumbRY; } InputReport;
typedef struct { unsigned device_index; InputReport in_state; } USBXIDGamepadState;
static ControllerState native[2];
static ControllerState *bound_controllers[4]={&native[0],&native[1],NULL,NULL};
static struct { bool engaged; uint16_t buttons; int16_t axes[CONTROLLER_AXIS__COUNT]; } input;
static bool held, test_mode;
static int polls;
static int64_t host_now=1000000;
static uint16_t physical_buttons=CONTROLLER_BUTTON_B;
static int64_t qemu_clock_get_us(int clock) { (void)clock; return host_now; }
static void poll_native(ControllerState *state) { polls++; state->buttons=physical_buttons; }
static void xemu_input_update_sdl_kbd_controller_state(ControllerState *s) { poll_native(s); }
static void xemu_input_update_sdl_controller_state(ControllerState *s) { poll_native(s); }
static bool xemu_input_get_test_mode(void) { return test_mode; }
static ControllerState *xemu_input_get_bound(unsigned port) { return bound_controllers[port]; }
'''+apply+'\n'+update+'\n'+report+r'''
int main(void) {
    USBXIDGamepadState pad={0},other={.device_index=1};
    native[0].type=INPUT_DEVICE_SDL_KEYBOARD;
    native[0].buttons=CONTROLLER_BUTTON_B; native[0].last_input_updated_ts=host_now;
    native[1]=native[0];
    input.engaged=true;input.buttons=CONTROLLER_BUTTON_A|CONTROLLER_BUTTON_DPAD_UP;
    input.axes[CONTROLLER_AXIS_LTRIG]=16384;
    input.axes[CONTROLLER_AXIS_LSTICK_X]=-1234;
    update_input(&pad);
    assert(pad.in_state.bAnalogButtons[GAMEPAD_A]==255);
    assert(pad.in_state.bAnalogButtons[GAMEPAD_B]==0);
    assert(pad.in_state.wButtons==BUTTON_MASK(GAMEPAD_DPAD_UP));
    assert(pad.in_state.bAnalogButtons[GAMEPAD_LEFT_TRIGGER]==128 && pad.in_state.sThumbLX==-1234);
    assert(polls==0 && native[0].buttons==CONTROLLER_BUTTON_B && native[0].axis[0]==0);
    /* New managed values and release at exactly the same host timestamp. */
    input.buttons=CONTROLLER_BUTTON_X;update_input(&pad);
    assert(pad.in_state.bAnalogButtons[GAMEPAD_X]==255 && pad.in_state.bAnalogButtons[GAMEPAD_A]==0);
    input.engaged=false;update_input(&pad);
    assert(pad.in_state.bAnalogButtons[GAMEPAD_X]==0 && pad.in_state.bAnalogButtons[GAMEPAD_B]==255);
    assert(pad.in_state.sThumbLX==0 && pad.in_state.bAnalogButtons[GAMEPAD_LEFT_TRIGGER]==0 && polls==0);
    /* Native cache updates remain native, including while an override is active. */
    host_now+=2501;physical_buttons=CONTROLLER_BUTTON_Y;input.engaged=true;input.buttons=CONTROLLER_BUTTON_A;
    update_input(&pad);assert(polls==1 && native[0].buttons==CONTROLLER_BUTTON_Y);
    input.engaged=false;update_input(&pad);
    assert(pad.in_state.bAnalogButtons[GAMEPAD_Y]==255 && pad.in_state.bAnalogButtons[GAMEPAD_A]==0 && polls==1);
    input.engaged=true;native[1].last_input_updated_ts=host_now;update_input(&other);
    assert(other.in_state.bAnalogButtons[GAMEPAD_B]==255 && other.in_state.bAnalogButtons[GAMEPAD_A]==0);
    InputReport before=pad.in_state;test_mode=true;update_input(&pad);
    assert(!memcmp(&before,&pad.in_state,sizeof(before)) && !held);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-input-report-') as directory:
    temp=Path(directory);(temp/'test.c').write_text(source)
    subprocess.run(['cc','-O2','-Wall','-Wextra','-Werror',str(temp/'test.c'),'-o',str(temp/'test')],check=True)
    subprocess.run([str(temp/'test')],check=True)
print('managed press/change/release, axes, native cache and other-port isolation passed')
