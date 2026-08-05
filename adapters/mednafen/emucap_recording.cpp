#include "emucap_recording.h"

#include <algorithm>
#include <cctype>
#include <cerrno>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <limits>
#include <sstream>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
static inline int recording_close_socket(int socket) {
  return ::closesocket(static_cast<SOCKET>(socket));
}
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
static inline int recording_close_socket(int socket) { return ::close(socket); }
#endif

namespace {

class SocketRecordingSink : public EmucapRecordingSink {
 public:
  explicit SocketRecordingSink(int socket) : socket_(socket) {}
  ~SocketRecordingSink() override {
    if (socket_ >= 0) recording_close_socket(socket_);
  }

  std::int64_t write(const char* data, std::size_t size, std::string& error) override {
#ifdef MSG_NOSIGNAL
    const int flags = MSG_NOSIGNAL;
#else
    const int flags = 0;
#endif
    const int sent = ::send(socket_, data, static_cast<int>(size), flags);
    if (sent < 0) error = std::strerror(errno);
    return sent;
  }

  bool close(std::string& error) override {
    if (socket_ < 0) return true;
    const int socket = socket_;
    socket_ = -1;
    if (recording_close_socket(socket) != 0) {
      error = std::strerror(errno);
      return false;
    }
    return true;
  }

 private:
  int socket_;
};

bool url_safe_token(const std::string& token) {
  if (token.size() < 16 || token.size() > 128) return false;
  for (const char value : token) {
    const unsigned char c = static_cast<unsigned char>(value);
    if (!(std::isalnum(c) || value == '-' || value == '_' || value == '.' || value == '~'))
      return false;
  }
  return true;
}

std::string event_line(
    std::uint64_t sequence,
    const char* event_class,
    const char* contract_sha256,
    std::uint64_t frame,
    std::uint64_t tick) {
  std::ostringstream out;
  out << "{\"sequence\":" << sequence
      << ",\"class\":\"" << event_class << "\",\"contract_sha256\":\""
      << contract_sha256
      << "\",\"clock\":{\"domain\":\"frame\",\"tick\":" << tick
      << "},\"frame\":" << frame << ",\"payload\":{}}\n";
  return out.str();
}

bool normalized_event_array(const std::string& line, std::string& normalized) {
  const std::string key = "\"event_classes\"";
  const std::size_t key_pos = line.find(key);
  if (key_pos == std::string::npos ||
      line.find(key, key_pos + key.size()) != std::string::npos) return false;
  const std::size_t colon = line.find(':', key_pos + key.size());
  if (colon == std::string::npos) return false;
  std::size_t begin = colon + 1;
  while (begin < line.size() && std::isspace(static_cast<unsigned char>(line[begin]))) begin++;
  if (begin >= line.size() || line[begin] != '[') return false;

  bool quoted = false;
  bool escaped = false;
  unsigned depth = 0;
  std::size_t end = std::string::npos;
  for (std::size_t pos = begin; pos < line.size(); pos++) {
    const char value = line[pos];
    if (quoted) {
      if (escaped) escaped = false;
      else if (value == '\\') escaped = true;
      else if (value == '"') quoted = false;
      continue;
    }
    if (value == '"') quoted = true;
    else if (value == '[') depth++;
    else if (value == ']') {
      if (depth == 0) return false;
      depth--;
      if (depth == 0) {
        end = pos;
        break;
      }
    }
  }
  if (end == std::string::npos || quoted) return false;

  normalized.clear();
  quoted = false;
  escaped = false;
  for (std::size_t pos = begin; pos <= end; pos++) {
    const char value = line[pos];
    if (quoted) {
      normalized += value;
      if (escaped) escaped = false;
      else if (value == '\\') escaped = true;
      else if (value == '"') quoted = false;
    } else if (value == '"') {
      quoted = true;
      normalized += value;
    } else if (!std::isspace(static_cast<unsigned char>(value))) {
      normalized += value;
    }
  }
  return true;
}

bool parse_event_object(
    const std::string& object,
    const char* id,
    const char* digest) {
  const std::string id_first =
      "{\"id\":\"" + std::string(id) + "\",\"contract_sha256\":\"" + digest + "\"}";
  const std::string digest_first =
      "{\"contract_sha256\":\"" + std::string(digest) + "\",\"id\":\"" + id + "\"}";
  return object == id_first || object == digest_first;
}

}  // namespace

std::unique_ptr<EmucapRecordingSink> emucap_open_recording_sink(
    const std::string& endpoint,
    const std::string& token,
    const std::string& capture_id,
    std::string& error) {
  const std::string prefix = "127.0.0.1:";
  if (endpoint.compare(0, prefix.size(), prefix) != 0 || !url_safe_token(token)) {
    error = "sink endpoint or token is invalid";
    return std::unique_ptr<EmucapRecordingSink>();
  }
  char* end = nullptr;
  const long port = std::strtol(endpoint.c_str() + prefix.size(), &end, 10);
  if (!end || *end != '\0' || port < 1 || port > 65535) {
    error = "sink endpoint port is invalid";
    return std::unique_ptr<EmucapRecordingSink>();
  }

  const int socket = static_cast<int>(::socket(AF_INET, SOCK_STREAM, 0));
  if (socket < 0) {
    error = "sink socket creation failed";
    return std::unique_ptr<EmucapRecordingSink>();
  }
#ifdef SO_NOSIGPIPE
  {
    const int one = 1;
    setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
  }
#endif
#ifdef _WIN32
  {
    const DWORD timeout_ms = 100;
    setsockopt(
        static_cast<SOCKET>(socket), SOL_SOCKET, SO_SNDTIMEO,
        reinterpret_cast<const char*>(&timeout_ms), sizeof(timeout_ms));
  }
#else
  {
    struct timeval timeout;
    timeout.tv_sec = 0;
    timeout.tv_usec = 100000;
    setsockopt(socket, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
  }
#endif
  struct sockaddr_in address;
  std::memset(&address, 0, sizeof(address));
  address.sin_family = AF_INET;
  address.sin_port = htons(static_cast<unsigned short>(port));
  inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
  if (::connect(socket, reinterpret_cast<struct sockaddr*>(&address), sizeof(address)) != 0) {
    error = "sink connection failed";
    recording_close_socket(socket);
    return std::unique_ptr<EmucapRecordingSink>();
  }

  std::unique_ptr<EmucapRecordingSink> sink(new SocketRecordingSink(socket));
  const std::string handshake =
      "{\"token\":\"" + token + "\",\"capture_id\":\"" + capture_id + "\"}\n";
  std::string write_error;
  if (sink->write(handshake.data(), handshake.size(), write_error) !=
      static_cast<std::int64_t>(handshake.size())) {
    error = "sink authentication write failed: " + write_error;
    return std::unique_ptr<EmucapRecordingSink>();
  }
  return sink;
}

const char* emucap_recording_capability_revision(bool reset_release) {
  return reset_release
      ? EMUCAP_RECORDING_RESET_CAPABILITY_REVISION
      : EMUCAP_RECORDING_CAPABILITY_REVISION;
}

std::string emucap_recording_capability_json(bool reset_release) {
  const std::string origins = reset_release
      ? "[\"next_frame_boundary\",\"reset_release\"]"
      : "[\"next_frame_boundary\"]";
  return std::string("{\"revision\":\"") +
      emucap_recording_capability_revision(reset_release) +
      "\",\"origins\":" + origins + ",\"units\":[\"frames\"],"
      "\"default_event_classes\":[\"frame_boundary\"],\"event_classes\":[{\"id\":"
      "\"frame_boundary\",\"contract_sha256\":\"" +
      EMUCAP_RECORDING_FRAME_CONTRACT_SHA256 +
      "\",\"clock_domains\":[\"frame\"],\"exact\":true},{\"id\":"
      "\"frame_completed\",\"contract_sha256\":\"" +
      EMUCAP_RECORDING_COMPLETED_CONTRACT_SHA256 +
      "\",\"clock_domains\":[\"frame\"],\"exact\":true,\"stoppable\":true}],"
      "\"input_movie\":{\"format\":\"" + EMUCAP_RECORDING_INPUT_MOVIE_FORMAT +
      "\",\"port\":0,\"max_frames\":300,\"max_bytes\":1048576,"
      "\"max_buttons_per_frame\":32},\"limits\":{"
      "\"max_frames\":300,\"max_events\":100000,\"max_bytes\":67108864,"
      "\"max_line_bytes\":65536,\"max_host_ms\":30000,"
      "\"progress_interval_ms\":250}}";
}

bool emucap_recording_exact_event_classes(
    const std::string& line,
    bool& include_frame_completed) {
  std::string normalized;
  if (!normalized_event_array(line, normalized) || normalized.size() < 4) return false;
  const std::string contents = normalized.substr(1, normalized.size() - 2);
  std::vector<std::string> objects;
  unsigned depth = 0;
  std::size_t begin = 0;
  for (std::size_t pos = 0; pos < contents.size(); pos++) {
    if (contents[pos] == '{') {
      if (depth++ == 0) begin = pos;
    } else if (contents[pos] == '}') {
      if (depth == 0) return false;
      if (--depth == 0) objects.push_back(contents.substr(begin, pos - begin + 1));
    } else if (depth == 0 && contents[pos] != ',') {
      return false;
    }
  }
  if (depth != 0 || objects.empty() || objects.size() > 2) return false;

  bool boundary = false;
  include_frame_completed = false;
  for (const std::string& object : objects) {
    if (parse_event_object(
            object, "frame_boundary", EMUCAP_RECORDING_FRAME_CONTRACT_SHA256)) {
      if (boundary) return false;
      boundary = true;
    } else if (parse_event_object(
                   object, "frame_completed",
                   EMUCAP_RECORDING_COMPLETED_CONTRACT_SHA256)) {
      if (include_frame_completed) return false;
      include_frame_completed = true;
    } else {
      return false;
    }
  }
  return boundary;
}

bool emucap_parse_recording_input_movie(
    const std::string& text,
    std::uint64_t frames,
    const EmucapRecordingButtonLookup& lookup,
    std::vector<std::uint16_t>& masks,
    std::string& error) {
  if (!lookup || frames < 1 || text.empty() || text.back() != '\n') {
    error = "input_movie parser arguments are invalid";
    return false;
  }
  masks.clear();
  masks.reserve(static_cast<std::size_t>(frames));
  std::size_t cursor = 0;
  for (std::uint64_t expected = 0; expected < frames; expected++) {
    const std::size_t newline = text.find('\n', cursor);
    if (newline == std::string::npos) {
      error = "input_movie frame count mismatch";
      return false;
    }
    const std::string row = text.substr(cursor, newline - cursor);
    const std::string prefix = std::to_string(expected) + ":";
    if (row.compare(0, prefix.size(), prefix) != 0) {
      error = "input_movie offsets must be dense from zero";
      return false;
    }
    const std::string names = row.substr(prefix.size());
    std::uint16_t mask = 0;
    std::string previous;
    std::size_t button_count = 0;
    std::size_t begin = 0;
    while (begin < names.size()) {
      const std::size_t comma = names.find(',', begin);
      const std::size_t end = comma == std::string::npos ? names.size() : comma;
      const std::string button = names.substr(begin, end - begin);
      if (button.empty() || (!previous.empty() && button <= previous)) {
        error = "input_movie buttons are not canonical";
        return false;
      }
      for (const unsigned char value : button) {
        if (!((value >= 'a' && value <= 'z') || (value >= '0' && value <= '9')
              || value == '_' || value == '+' || value == '-')) {
          error = "input_movie buttons are not canonical";
          return false;
        }
      }
      std::uint16_t bit = 0;
      if (!lookup(button, bit) || bit == 0) {
        error = "input_movie contains an unsupported button: " + button;
        return false;
      }
      if ((mask & bit) != 0) {
        error = "input_movie contains aliases for the same button";
        return false;
      }
      mask = static_cast<std::uint16_t>(mask | bit);
      previous = button;
      if (++button_count > 32) {
        error = "input_movie button count exceeds the advertised limit";
        return false;
      }
      if (comma == std::string::npos) break;
      begin = comma + 1;
      if (begin == names.size()) {
        error = "input_movie buttons are not canonical";
        return false;
      }
    }
    masks.push_back(mask);
    cursor = newline + 1;
  }
  if (cursor != text.size()) {
    error = "input_movie frame count mismatch";
    return false;
  }
  return true;
}

EmucapRecording::EmucapRecording(
    const EmucapRecordingRequest& request,
    std::uint64_t current_boundary,
    std::uint64_t now_ms,
    std::unique_ptr<EmucapRecordingSink> sink,
    EmucapRecordingInputHandler input_handler)
    : request_(request),
      sink_(std::move(sink)),
      input_handler_(input_handler),
      started_ms_(now_ms),
      last_progress_ms_(now_ms),
      last_boundary_(current_boundary),
      f_start_(current_boundary),
      f_end_(current_boundary),
      final_frame_(current_boundary) {}

EmucapRecording::~EmucapRecording() { close_sink(); }

bool EmucapRecording::close_sink() {
  if (sink_released_) return true;
  if (sink_close_attempted_) return false;
  sink_close_attempted_ = true;
  std::string error;
  if (!sink_ || !sink_->close(error)) {
    status_ = "failed";
    operation_outcome_ = "failed";
    execution_outcome_ = "adapter_error";
    integrity_ = "unverifiable";
    reason_ = "sink_close_failed: " + error;
    return false;
  }
  sink_released_ = true;
  return true;
}

EmucapRecordingEffect EmucapRecording::terminal(
    const char* status,
    const char* operation,
    const char* execution,
    const char* integrity,
    std::uint64_t boundary,
    const std::string& reason,
    const char* final_execution_state) {
  active_ = false;
  status_ = status;
  operation_outcome_ = operation;
  execution_outcome_ = execution;
  integrity_ = integrity;
  final_frame_ = boundary;
  reason_ = reason;
  final_execution_state_ = final_execution_state;
  std::string input_error;
  if (!release_input(input_error)) {
    status_ = "failed";
    operation_outcome_ = "failed";
    execution_outcome_ = "adapter_error";
    integrity_ = "unverifiable";
    reason_ = reason_.empty()
        ? "input_release_failed: " + input_error
        : reason_ + "; input_release_failed: " + input_error;
  }
  close_sink();
  return EmucapRecordingEffect::terminal;
}

bool EmucapRecording::release_input(std::string& error) {
  if (!input_acquired_ || input_released_) return true;
  if (!input_handler_ || !input_handler_(false, 0, error)) return false;
  input_released_ = true;
  return true;
}

EmucapRecordingEffect EmucapRecording::apply_movie_frame(
    std::uint64_t offset,
    std::uint64_t boundary) {
  if (request_.input_masks.empty()) return EmucapRecordingEffect::none;
  if (offset >= request_.input_masks.size() || !input_handler_) {
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", boundary,
        "input_movie_state_unavailable", "frozen");
  }
  std::string error;
  if (!input_handler_(true, request_.input_masks[static_cast<std::size_t>(offset)], error)) {
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", boundary,
        "input_apply_failed: " + error, "frozen");
  }
  input_acquired_ = true;
  input_released_ = false;
  return EmucapRecordingEffect::none;
}

EmucapRecordingEffect EmucapRecording::write_event(
    const char* event_class,
    const char* contract_sha256,
    std::uint64_t frame,
    std::uint64_t tick) {
  const bool is_boundary = std::strcmp(event_class, "frame_boundary") == 0;
  const std::uint64_t expected = f_start_ +
      (is_boundary ? boundary_records_ : completed_records_);
  if (frame != expected) {
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", tick,
        is_boundary ? "frame_boundary_gap_or_regression"
                    : "frame_completed_gap_or_regression",
        "frozen");
  }
  const std::string line = event_line(events_, event_class, contract_sha256, frame, tick);
  if (line.size() > request_.limits.max_line_bytes) {
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", tick,
        "event_line_limit_exceeded", "frozen");
  }
  if (events_ + 1 > request_.limits.max_events) {
    ++dropped_;
    return terminal(
        "failed", "failed", "loss_detected", "lossy", tick,
        "event_limit_exceeded", "frozen");
  }
  if (line.size() > request_.limits.max_bytes ||
      physical_bytes_ > request_.limits.max_bytes - line.size()) {
    ++dropped_;
    return terminal(
        "failed", "failed", "loss_detected", "lossy", tick,
        "byte_limit_exceeded", "frozen");
  }
  std::string error;
  const std::int64_t sent = sink_->write(line.data(), line.size(), error);
  if (sent != static_cast<std::int64_t>(line.size())) {
    if (sent > 0) {
      physical_bytes_ += static_cast<std::uint64_t>(sent);
      truncated_ = true;
    }
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", tick,
        "sink_write_failed: " + error, "frozen");
  }
  ++events_;
  bytes_ += line.size();
  physical_bytes_ += line.size();
  if (is_boundary) {
    ++boundary_records_;
    last_boundary_ = frame;
  } else {
    ++completed_records_;
  }
  return EmucapRecordingEffect::none;
}

EmucapRecordingEffect EmucapRecording::tick(
    std::uint64_t boundary,
    std::uint64_t now_ms) {
  if (!active_) return EmucapRecordingEffect::none;
  if (now_ms < started_ms_ || now_ms - started_ms_ > request_.limits.max_host_ms) {
    return terminal(
        "interrupted", "aborted", "adapter_error", "unverifiable", boundary,
        "host_deadline_exceeded", "frozen");
  }

  if (!initial_boundary_written_) {
    if (boundary != f_start_) {
      return terminal(
          "failed", "failed", "adapter_error", "unverifiable", boundary,
          "initial_boundary_mismatch", "frozen");
    }
    const EmucapRecordingEffect input = apply_movie_frame(0, boundary);
    if (input == EmucapRecordingEffect::terminal) return input;
    const EmucapRecordingEffect event = write_event(
        "frame_boundary", EMUCAP_RECORDING_FRAME_CONTRACT_SHA256, boundary, boundary);
    if (event == EmucapRecordingEffect::terminal) return event;
    initial_boundary_written_ = true;
    return EmucapRecordingEffect::none;
  }

  const std::uint64_t expected = f_start_ + completed_frames_ + 1;
  if (boundary != expected || completed_frames_ >= request_.frames) {
    return terminal(
        "failed", "failed", "adapter_error", "unverifiable", boundary,
        "end_boundary_missed_or_regressed", "frozen");
  }

  // Reaching this callback proves the preceding guest interval completed, even if writing its
  // completion event fails. Preserve that execution fact separately from stream integrity.
  ++completed_frames_;
  f_end_ = boundary;
  final_frame_ = boundary;

  if (request_.include_frame_completed) {
    const std::uint64_t sequence = events_;
    const EmucapRecordingEffect completion = write_event(
        "frame_completed", EMUCAP_RECORDING_COMPLETED_CONTRACT_SHA256,
        boundary - 1, boundary);
    if (completion == EmucapRecordingEffect::terminal) return completion;
    if (request_.stop_on_frame_completed &&
        completed_records_ == request_.stop_occurrence) {
      has_stop_event_ = true;
      stop_sequence_ = sequence;
      stop_clock_tick_ = boundary;
      stop_frame_ = boundary - 1;
      stop_occurrence_ = completed_records_;
    }
  }

  if (has_stop_event_) {
    return terminal(
        "completed", "completed", "event_stop", "complete", boundary, "", "frozen");
  }
  if (completed_frames_ == request_.frames) {
    const std::uint64_t expected_events = request_.frames *
        (request_.include_frame_completed ? 2 : 1);
    if (events_ != expected_events || boundary_records_ != request_.frames ||
        (request_.include_frame_completed && completed_records_ != request_.frames)) {
      return terminal(
          "failed", "failed", "loss_detected", "lossy", boundary,
          "incomplete_interval", "frozen");
    }
    return terminal(
        "completed", "completed", "target_reached", "complete", boundary, "", "frozen");
  }

  const EmucapRecordingEffect input = apply_movie_frame(completed_frames_, boundary);
  if (input == EmucapRecordingEffect::terminal) return input;
  const EmucapRecordingEffect next_boundary = write_event(
      "frame_boundary", EMUCAP_RECORDING_FRAME_CONTRACT_SHA256, boundary, boundary);
  if (next_boundary == EmucapRecordingEffect::terminal) return next_boundary;
  if (now_ms < last_progress_ms_ ||
      now_ms - last_progress_ms_ >= request_.limits.progress_interval_ms) {
    last_progress_ms_ = now_ms;
    return EmucapRecordingEffect::working;
  }
  return EmucapRecordingEffect::none;
}

EmucapRecordingEffect EmucapRecording::cancel(
    std::uint64_t boundary,
    std::uint64_t now_ms,
    const std::string& reason,
    const std::string& final_execution_state) {
  (void)now_ms;
  if (!active_) return EmucapRecordingEffect::none;
  return terminal(
      "interrupted", "aborted", "adapter_error", "unverifiable", boundary, reason,
      final_execution_state.c_str());
}

std::string EmucapRecording::progress_json() {
  std::ostringstream out;
  out << "{\"status\":\"working\",\"capture_id\":\"" << request_.capture_id
      << "\",\"sequence\":" << progress_sequence_++ << ",\"frame\":" << last_boundary_
      << ",\"frames\":" << completed_frames_
      << ",\"events\":" << events_ << ",\"bytes\":" << bytes_ << "}";
  return out.str();
}

EmucapRecordingResult EmucapRecording::result(std::uint64_t now_ms) const {
  EmucapRecordingResult result;
  result.status = status_;
  result.operation_outcome = operation_outcome_;
  result.execution_outcome = execution_outcome_;
  result.integrity = integrity_;
  result.reason = reason_;
  result.final_execution_state = final_execution_state_;
  result.f_start = f_start_;
  result.f_end = f_end_;
  result.final_frame = final_frame_;
  result.frames = completed_frames_;
  result.events = events_;
  result.bytes = bytes_;
  result.physical_bytes = physical_bytes_;
  result.dropped = dropped_;
  result.truncated = truncated_;
  result.wall_ms = now_ms >= started_ms_ ? now_ms - started_ms_ : 0;
  result.input_acquired = input_acquired_;
  result.input_released = input_released_;
  result.sink_released = sink_released_;
  result.has_stop_event = has_stop_event_;
  result.stop_sequence = stop_sequence_;
  result.stop_clock_tick = stop_clock_tick_;
  result.stop_frame = stop_frame_;
  result.stop_occurrence = stop_occurrence_;
  return result;
}
