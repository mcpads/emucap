#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <vector>

static const char* const EMUCAP_RECORDING_FRAME_CONTRACT_SHA256 =
    "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd";
static const char* const EMUCAP_RECORDING_COMPLETED_CONTRACT_SHA256 =
    "a335a785a0c109cc7edc6ecab27ff429e386c2ad2eb34769cac4f9cc47378b91";
static const char* const EMUCAP_RECORDING_CAPABILITY_REVISION =
    "fb6acdcc8e5d8a9ff21f49f22c2d08425d414d5d39af52ee04553a578a0b1224";
static const char* const EMUCAP_RECORDING_RESET_CAPABILITY_REVISION =
    "7daa3baefbe5f7b3455d52827349ff943841e6f5e3b572aacd1ef94245b85272";
static const char* const EMUCAP_RECORDING_INPUT_MOVIE_FORMAT = "frame-full-state-1";

struct EmucapRecordingLimits {
  std::uint64_t max_frames = 0;
  std::uint64_t max_events = 0;
  std::uint64_t max_bytes = 0;
  std::uint64_t max_line_bytes = 0;
  std::uint64_t max_host_ms = 0;
  std::uint64_t progress_interval_ms = 0;
};

struct EmucapRecordingRequest {
  long request_id = -1;
  std::string capture_id;
  std::string launch_id;
  std::string request_digest_sha256;
  std::uint64_t frames = 0;
  bool reset_release = false;
  bool include_frame_completed = false;
  bool stop_on_frame_completed = false;
  std::uint64_t stop_occurrence = 0;
  std::vector<std::uint16_t> input_masks;
  EmucapRecordingLimits limits;
};

using EmucapRecordingInputHandler =
    std::function<bool(bool engaged, std::uint16_t mask, std::string& error)>;
using EmucapRecordingButtonLookup =
    std::function<bool(const std::string& name, std::uint16_t& bit)>;

class EmucapRecordingSink {
 public:
  virtual ~EmucapRecordingSink() {}
  virtual std::int64_t write(const char* data, std::size_t size, std::string& error) = 0;
  virtual bool close(std::string& error) = 0;
};

std::unique_ptr<EmucapRecordingSink> emucap_open_recording_sink(
    const std::string& endpoint,
    const std::string& token,
    const std::string& capture_id,
    std::string& error);

std::string emucap_recording_capability_json(bool reset_release);
const char* emucap_recording_capability_revision(bool reset_release);
bool emucap_recording_exact_event_classes(
    const std::string& line,
    bool& include_frame_completed);
bool emucap_parse_recording_input_movie(
    const std::string& text,
    std::uint64_t frames,
    const EmucapRecordingButtonLookup& lookup,
    std::vector<std::uint16_t>& masks,
    std::string& error);

enum class EmucapRecordingEffect {
  none,
  working,
  terminal,
};

struct EmucapRecordingResult {
  std::string status;
  std::string operation_outcome;
  std::string execution_outcome;
  std::string integrity;
  std::string reason;
  std::string final_execution_state;
  std::uint64_t f_start = 0;
  std::uint64_t f_end = 0;
  std::uint64_t final_frame = 0;
  std::uint64_t frames = 0;
  std::uint64_t events = 0;
  std::uint64_t bytes = 0;
  std::uint64_t physical_bytes = 0;
  std::uint64_t dropped = 0;
  bool truncated = false;
  std::uint64_t wall_ms = 0;
  bool input_acquired = false;
  bool input_released = false;
  bool sink_released = false;
  bool has_stop_event = false;
  std::uint64_t stop_sequence = 0;
  std::uint64_t stop_clock_tick = 0;
  std::uint64_t stop_frame = 0;
  std::uint64_t stop_occurrence = 0;
};

class EmucapRecording {
 public:
  EmucapRecording(
      const EmucapRecordingRequest& request,
      std::uint64_t current_boundary,
      std::uint64_t now_ms,
      std::unique_ptr<EmucapRecordingSink> sink,
      EmucapRecordingInputHandler input_handler = EmucapRecordingInputHandler());
  ~EmucapRecording();

  EmucapRecordingEffect tick(std::uint64_t boundary, std::uint64_t now_ms);
  EmucapRecordingEffect cancel(
      std::uint64_t boundary,
      std::uint64_t now_ms,
      const std::string& reason,
      const std::string& final_execution_state);
  bool active() const { return active_; }
  long request_id() const { return request_.request_id; }
  const std::string& capture_id() const { return request_.capture_id; }
  const std::string& launch_id() const { return request_.launch_id; }
  std::string progress_json();
  EmucapRecordingResult result(std::uint64_t now_ms) const;

 private:
  EmucapRecordingEffect write_event(
      const char* event_class,
      const char* contract_sha256,
      std::uint64_t frame,
      std::uint64_t tick);
  EmucapRecordingEffect apply_movie_frame(std::uint64_t offset, std::uint64_t boundary);
  bool release_input(std::string& error);
  EmucapRecordingEffect terminal(
      const char* status,
      const char* operation,
      const char* execution,
      const char* integrity,
      std::uint64_t boundary,
      const std::string& reason,
      const char* final_execution_state);
  bool close_sink();

  EmucapRecordingRequest request_;
  std::unique_ptr<EmucapRecordingSink> sink_;
  EmucapRecordingInputHandler input_handler_;
  bool active_ = true;
  bool sink_close_attempted_ = false;
  bool sink_released_ = false;
  std::uint64_t started_ms_ = 0;
  std::uint64_t last_progress_ms_ = 0;
  std::uint64_t progress_sequence_ = 0;
  std::uint64_t last_boundary_ = 0;
  std::uint64_t f_start_ = 0;
  std::uint64_t f_end_ = 0;
  std::uint64_t final_frame_ = 0;
  std::uint64_t events_ = 0;
  std::uint64_t bytes_ = 0;
  std::uint64_t physical_bytes_ = 0;
  std::uint64_t completed_frames_ = 0;
  std::uint64_t boundary_records_ = 0;
  std::uint64_t completed_records_ = 0;
  std::uint64_t dropped_ = 0;
  bool truncated_ = false;
  bool initial_boundary_written_ = false;
  bool input_acquired_ = false;
  bool input_released_ = false;
  bool has_stop_event_ = false;
  std::uint64_t stop_sequence_ = 0;
  std::uint64_t stop_clock_tick_ = 0;
  std::uint64_t stop_frame_ = 0;
  std::uint64_t stop_occurrence_ = 0;
  std::string status_ = "working";
  std::string operation_outcome_;
  std::string execution_outcome_;
  std::string integrity_;
  std::string reason_;
  std::string final_execution_state_ = "unknown";
};
