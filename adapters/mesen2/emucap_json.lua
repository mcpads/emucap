-- Strict request decoding. Unsigned identities outside Lua's signed integer range
-- retain their decimal token instead of passing through a rounded floating value.
local M = {}
local U64 = {}
local OBJECT, ARRAY = {}, {}
M.null = setmetatable({}, {})
function M.is_object(value) return type(value) == "table" and getmetatable(value) == OBJECT end
function M.is_array(value) return type(value) == "table" and getmetatable(value) == ARRAY end

function M.unsigned_text(value)
  if type(value) == "table" and getmetatable(value) == U64 then return value.text end
  if math.type(value) == "integer" and value >= 0 then return tostring(value) end
  return nil
end

function M.raw_unsigned(value)
  if type(value) == "table" and getmetatable(value) == U64 then return value.text end
end

function M.decode(s)
  assert(type(s) == "string", "JSON input must be a string")
  local i, length = 1, #s
  local value
  local function fail() error("invalid JSON at byte " .. tostring(i), 0) end
  local function ws()
    while s:sub(i, i):match("[ \t\r\n]") do i = i + 1 end
  end
  local function hex4()
    local text = s:sub(i, i + 3)
    if #text ~= 4 or text:find("[^0-9a-fA-F]") then fail() end
    i = i + 4
    return tonumber(text, 16)
  end
  local escapes = { ['"'] = '"', ['\\'] = '\\', ['/'] = '/',
    b = '\b', f = '\f', n = '\n', r = '\r', t = '\t' }
  local function str()
    if s:sub(i, i) ~= '"' then fail() end
    i = i + 1
    local parts, start = {}, i
    while i <= length do
      local c = s:sub(i, i)
      if c == '"' then
        parts[#parts + 1] = s:sub(start, i - 1)
        i = i + 1
        local result = table.concat(parts)
        if not utf8.len(result) then fail() end
        return result
      elseif c == '\\' then
        parts[#parts + 1] = s:sub(start, i - 1)
        i = i + 1
        local e = s:sub(i, i)
        i = i + 1
        if e == 'u' then
          local cp = hex4()
          if cp >= 0xd800 and cp <= 0xdbff then
            if s:sub(i, i + 1) ~= '\\u' then fail() end
            i = i + 2
            local low = hex4()
            if low < 0xdc00 or low > 0xdfff then fail() end
            cp = 0x10000 + (cp - 0xd800) * 0x400 + low - 0xdc00
          elseif cp >= 0xdc00 and cp <= 0xdfff then fail() end
          parts[#parts + 1] = utf8.char(cp)
        else
          if not escapes[e] then fail() end
          parts[#parts + 1] = escapes[e]
        end
        start = i
      else
        if c:byte() < 32 then fail() end
        i = i + 1
      end
    end
    fail()
  end
  local function number()
    local start = i
    if s:sub(i, i) == '-' then i = i + 1 end
    local digit = s:sub(i, i)
    if digit == '0' then i = i + 1
    elseif digit:match('[1-9]') then
      repeat i = i + 1 until not s:sub(i, i):match('%d')
    else fail() end
    local integral = true
    if s:sub(i, i) == '.' then
      integral = false
      i = i + 1
      if not s:sub(i, i):match('%d') then fail() end
      repeat i = i + 1 until not s:sub(i, i):match('%d')
    end
    if s:sub(i, i):match('[eE]') then
      integral = false
      i = i + 1
      if s:sub(i, i):match('[+-]') then i = i + 1 end
      if not s:sub(i, i):match('%d') then fail() end
      repeat i = i + 1 until not s:sub(i, i):match('%d')
    end
    local token = s:sub(start, i - 1)
    local n = tonumber(token)
    if not n or n ~= n or n == math.huge or n == -math.huge then fail() end
    if integral and math.type(n) ~= 'integer' then
      if token:sub(1, 1) == '-' or #token > 20
        or (#token == 20 and token > '18446744073709551615') then fail() end
      return setmetatable({ text = token }, U64)
    end
    return n
  end
  value = function(depth)
    if depth > 64 then fail() end
    ws()
    local c = s:sub(i, i)
    if c == '"' then return str() end
    if c == '{' or c == '[' then
      local object = c == '{'
      local closing = object and '}' or ']'
      local result, seen = setmetatable({}, object and OBJECT or ARRAY), {}
      i = i + 1
      ws()
      if s:sub(i, i) == closing then i = i + 1; return result end
      while true do
        local key = #result + 1
        if object then
          ws(); key = str(); ws()
          if seen[key] or s:sub(i, i) ~= ':' then fail() end
          seen[key] = true
          i = i + 1
        end
        result[key] = value(depth + 1)
        ws()
        c = s:sub(i, i)
        i = i + 1
        if c == closing then return result end
        if c ~= ',' then fail() end
      end
    end
    for _, literal in ipairs({ 'true', 'false', 'null' }) do
      if s:sub(i, i + #literal - 1) == literal then
        i = i + #literal
        if literal == 'null' then return M.null end
        return literal == 'true'
      end
    end
    return number()
  end
  local result = value(0)
  ws()
  if i ~= length + 1 then fail() end
  return result
end

return M
