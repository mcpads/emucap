-- Production deferred-state regression test.
-- Run: EMUCAP_ADAPTER_DIR=. lua flush_deferred_test.lua

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local Deferred = require("emucap_deferred")

local function eq(actual, expected, message)
  if actual ~= expected then
    error(("FAIL %s: %s ~= %s"):format(message, tostring(actual), tostring(expected)))
  end
end

local function effect_order(effect)
  local order = {}
  Deferred.apply(effect, {
    release_input = function(value)
      order[#order + 1] = "release:" .. value.operation_kind
    end,
    working = function(value)
      order[#order + 1] = "working:" .. value.id
    end,
    terminal = function(value)
      order[#order + 1] = "terminal:" .. value.id
    end,
    cancelled = function(value)
      order[#order + 1] = "cancelled:" .. value.id
    end,
  })
  return table.concat(order, ",")
end

-- A completed press returns input ownership before exposing its terminal response.
do
  local state = Deferred.start(3, 42, "press", 1)
  local next_state, effect = Deferred.tick(state, 500, 30)
  eq(next_state, nil, "press completion clears state")
  eq(effect.session, 3, "press session")
  eq(effect.id, 42, "press response id")
  eq(effect.status, "completed", "press completion status")
  eq(effect.frame, 500, "press completion frame")
  eq(effect_order(effect), "release:press,terminal:42", "press completion effect order")

  local after_state, after_effect = Deferred.tick(next_state, 501, 30)
  eq(after_state, nil, "completed press remains idle")
  eq(after_effect, nil, "completed press has no second terminal")
end

-- A breakpoint interruption preserves evidence and has the same release-before-terminal order.
do
  local state = Deferred.start(4, 43, "press", 100)
  local next_state, effect =
    Deferred.interrupt(state, "interrupted", "breakpoint", 7, 510)
  eq(next_state, nil, "press interruption clears state")
  eq(effect.id, 43, "interrupted press response id")
  eq(effect.reason, "breakpoint", "interruption reason")
  eq(effect.breakpoint_id, 7, "interruption breakpoint id")
  eq(effect.frame, 510, "interruption frame")
  eq(effect_order(effect), "release:press,terminal:43", "press interruption effect order")
end

-- Run and probe complete without touching input ownership. Probe identity survives the transition.
do
  local run = Deferred.start(5, 7, "run", 1)
  local _, run_effect = Deferred.tick(run, 700, 30)
  eq(effect_order(run_effect), "terminal:7", "run completion effect order")

  local probe_spec = { address = 0x1234, length = 2 }
  local probe = Deferred.start(5, 8, "probe", 1, probe_spec)
  local _, probe_effect = Deferred.tick(probe, 701, 30)
  eq(probe_effect.probe, probe_spec, "probe specification identity")
  eq(effect_order(probe_effect), "terminal:8", "probe completion effect order")
end

-- Keepalive is non-terminal and preserves the active request.
do
  local state = Deferred.start(6, 9, "run", 2)
  state.age = 29
  local next_state, effect = Deferred.tick(state, 800, 30)
  eq(next_state.remaining, 1, "keepalive remaining")
  eq(next_state.age, 30, "keepalive age")
  eq(effect.status, "working", "keepalive status")
  eq(effect_order(effect), "working:9", "keepalive has no terminal or release")

  state = Deferred.start(6, 10, "press", 2)
  state.age = 29
  next_state, effect = Deferred.tick(state, 801, 30)
  eq(next_state.remaining, 1, "press keepalive remains active")
  eq(effect_order(effect), "working:10", "press keepalive retains transient input")
end

-- Disconnect cancels the response namespace and releases only a transient press.
do
  local press = Deferred.start(7, 11, "press", 4)
  local next_state, effect = Deferred.cancel(press)
  eq(next_state, nil, "cancelled press clears state")
  eq(effect_order(effect), "release:press,cancelled:11", "cancelled press effect order")

  local run = Deferred.start(7, 12, "run", 4)
  _, effect = Deferred.cancel(run)
  eq(effect_order(effect), "cancelled:12", "cancelled run does not release input")
end

-- Idle transitions are total and have no effects.
do
  local state, effect = Deferred.tick(nil, 900, 30)
  eq(state, nil, "idle tick state")
  eq(effect, nil, "idle tick effect")
  state, effect = Deferred.interrupt(nil, "interrupted", "breakpoint", 1, 900)
  eq(state, nil, "idle interrupt state")
  eq(effect, nil, "idle interrupt effect")
  state, effect = Deferred.cancel(nil)
  eq(state, nil, "idle cancel state")
  eq(effect, nil, "idle cancel effect")
end

print("ALL EMUCAP DEFERRED TESTS PASSED")
