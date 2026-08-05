local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Memory = require("emucap_memory")

local api = {
  memType = {
    nesMemory = 1,
    nesInternalRam = 2,
    snesWorkRam = 99,
  },
}
local sys = {
  default_memtype = "nesMemory",
  address_space_size = 0x10000,
  region_sizes = {
    nesInternalRam = 0x800,
    nesSaveRam = 0x2000,
  },
}

local catalog = Memory.catalog(api, sys)
assert(#catalog == 2)
assert(catalog[1] == "nesInternalRam")
assert(catalog[2] == "nesMemory")
local regions = Memory.regions(api, sys)
assert(#regions == 2)
assert(regions[1].memory_type == "nesInternalRam")
assert(regions[1].size == 0x800)
assert(regions[2].memory_type == "nesMemory")
assert(regions[2].size == 0x10000)

local bus = assert(Memory.range(api, sys, "nesMemory", 0xFFFF, 1))
assert(bus.memory_type == 1)
assert(bus.size == 0x10000)

local empty_tail = assert(Memory.range(api, sys, "nesInternalRam", 0x800, 0))
assert(empty_tail.length == 0)

local region, kind, message = Memory.range(api, sys, "nesInternalRam", 0x7FF, 2)
assert(region == nil)
assert(kind == "bad_params")
assert(message:match("exceeds region size"))

region, kind, message = Memory.range(api, sys, "snesWorkRam", 0, 1)
assert(region == nil)
assert(kind == "bad_params")
assert(message:match("unsupported or unavailable memory_type"))

region, kind = Memory.range(api, sys, "nesInternalRam", -1, 1)
assert(region == nil)
assert(kind == "bad_params")

region, kind = Memory.range(api, sys, "nesInternalRam", 0, 1.5)
assert(region == nil)
assert(kind == "bad_params")

local dynamic_api = {
  memType = api.memType,
  getMemorySize = function(memory_type)
    if memory_type == 1 then return 0x10000 end
    if memory_type == 2 then return 0x400 end
    return 0
  end,
}
local dynamic_catalog = Memory.catalog(dynamic_api, sys)
assert(#dynamic_catalog == 2)
local dynamic_regions = Memory.regions(dynamic_api, sys)
assert(dynamic_regions[1].memory_type == "nesInternalRam")
assert(dynamic_regions[1].size == 0x400)
local dynamic_ram = assert(Memory.range(dynamic_api, sys, "nesInternalRam", 0x3FF, 1))
assert(dynamic_ram.size == 0x400)
region, kind = Memory.range(dynamic_api, sys, "nesInternalRam", 0x3FF, 2)
assert(region == nil)
assert(kind == "bad_params")

local unavailable_api = {
  memType = api.memType,
  getMemorySize = function(memory_type)
    if memory_type == 1 then return 0x10000 end
    return 0
  end,
}
local unavailable_catalog = Memory.catalog(unavailable_api, sys)
assert(#unavailable_catalog == 1)
assert(unavailable_catalog[1] == "nesMemory")
region, kind = Memory.range(unavailable_api, sys, "nesInternalRam", 0, 1)
assert(region == nil)
assert(kind == "bad_params")

print("ALL MESEN MEMORY TESTS PASSED")
