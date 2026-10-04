#!/usr/bin/env python3
"""Load the built core through the same dynamic ABI used by the host."""
import ctypes
from pathlib import Path
import sys

core = ctypes.CDLL(str(Path(sys.argv[1]).resolve()))
core.emucap_np2_debug_api_version.restype = ctypes.c_uint32
core.retro_api_version.restype = ctypes.c_uint
assert core.emucap_np2_debug_api_version() == 3
assert core.retro_api_version() == 1
# Invalid requests must remain safe even before a guest is initialized.
core.emucap_np2_read_memory.argtypes = [ctypes.c_uint32, ctypes.c_void_p, ctypes.c_size_t]
core.emucap_np2_read_memory.restype = ctypes.c_int
assert core.emucap_np2_read_memory(0, None, 1) == 0
print('NP2kai dynamic debug and libretro ABI passed')
