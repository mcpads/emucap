#!/usr/bin/env python3
"""Actual batch handler and region resolver; controlled native byte getter."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/mesen2/emucap-core.lua').read_text()
handler = source.split('function handlers.read_memory_batch(p)', 1)[1].split('function handlers.hello()', 1)[0]
setup = r'''
package.loaded['emucap-core'] = true
local Memory = dofile('adapters/mesen2/emucap_memory.lua')
local handlers, Step = {}, {active=function() return false end}
local STATE, frame, boundary_seq, PORT = 'frozen', 7, 3, 0
local deferred, step_operation = nil, nil
local BATCH_MAX_RANGES, BATCH_MAX_BYTES, DEBUG_READ = 64, 65536, 0x100000
local function as_array(v) return v end
local reads, sizes = 0, {}
local emu = {memType={}}
function emu.getMemorySize(mt) return sizes[mt] end
function emu.read(address, view, side_effects)
  assert(view & DEBUG_READ ~= 0 and side_effects == false)
  local size = assert(sizes[view & ~DEBUG_READ])
  assert(address >= 0 and address < size)
  reads = reads + 1
  if address == 1 then return 0xa5 end
  if address == 2 then return 0xc3 end
  if address == size - 1 then return 0xe7 end
  return 0x42
end
'''
checks = r'''
local region_count = 0
for _, system in ipairs({'snes','nes','gb','gba','sms'}) do
  dofile('adapters/mesen2/emucap-' .. system .. '.lua')
  local catalog = {[SYS.default_memtype]=SYS.address_space_size}
  for name, size in pairs(SYS.region_sizes or {}) do catalog[name]=size end
  emu.memType, sizes = {}, {}
  local n = 0
  for name, size in pairs(catalog) do
    n=n+1; emu.memType[name]=n; sizes[n]=size
  end
  for _, region in ipairs(Memory.regions(emu,SYS)) do
    region_count=region_count+1
    local name, size = region.memory_type, region.size
    local function range(address,length)
      return {memory_type=name,address=address,length=length}
    end
    reads=0
    local ok, value=handlers.read_memory_batch({ranges={range(1,2),range(2,1),range(1,2),range(size-1,1)}})
    assert(ok and value.total_bytes==6 and reads==6)
    for i, expected in ipairs({'a5c3','c3','a5c3','e7'}) do
      assert(value.reads[i].index==i-1 and value.reads[i].hex==expected)
      assert(value.reads[i].memory_type==name)
    end
    for _, bad in ipairs({range(size-1,2),range(-1,1),range(0,0),range(0,1.5),range(math.maxinteger,2),{memory_type='absent',address=0,length=1}}) do
      reads=0
      assert(not handlers.read_memory_batch({ranges={range(1,2),bad}}))
      assert(reads==0,'late invalid range performed a payload read')
    end
    assert(STATE=='frozen' and frame==7 and boundary_seq==3)
  end
end
print('Mesen actual batch/resolver: '..region_count..' configured views, nonzero/overlap/duplicates/tail and late-rejection zero payload reads passed')
'''
with tempfile.TemporaryDirectory(prefix='mesen-batch-preflight-') as directory:
    script = Path(directory) / 'test.lua'
    script.write_text(setup + '\nfunction handlers.read_memory_batch(p)' + handler + checks)
    subprocess.run(['lua', str(script)], cwd=root, check=True)
