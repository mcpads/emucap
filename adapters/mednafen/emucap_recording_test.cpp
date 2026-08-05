#include "emucap_recording.h"
#include "emucap_input.h"

#include <cassert>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

class TestSink : public EmucapRecordingSink {
 public:
  explicit TestSink(
      std::size_t fail_after = static_cast<std::size_t>(-1),
      std::size_t partial_bytes = 0,
      std::vector<std::string>* order = nullptr)
      : fail_after_(fail_after), partial_bytes_(partial_bytes), order_(order) {}

  std::int64_t write(const char* data, std::size_t size, std::string& error) override {
    if (bytes_.size() >= fail_after_) {
      if (partial_bytes_ > 0) {
        const std::size_t sent = partial_bytes_ < size ? partial_bytes_ : size;
        bytes_.append(data, sent);
        return static_cast<std::int64_t>(sent);
      }
      error = "injected";
      return -1;
    }
    bytes_.append(data, size);
    if (order_) {
      order_->push_back(
          std::strstr(data, "\"class\":\"frame_completed\"")
              ? "event:completed"
              : "event:boundary");
    }
    return static_cast<std::int64_t>(size);
  }

  bool close(std::string& error) override {
    (void)error;
    closed_ = true;
    return true;
  }

  const std::string& bytes() const { return bytes_; }
  bool closed() const { return closed_; }
  void fail_after_current() { fail_after_ = bytes_.size(); }

 private:
  std::size_t fail_after_;
  std::size_t partial_bytes_;
  std::vector<std::string>* order_;
  std::string bytes_;
  bool closed_ = false;
};

EmucapRecordingRequest request(std::uint64_t frames) {
  EmucapRecordingRequest request;
  request.request_id = 7;
  request.capture_id = "capture-test";
  request.launch_id = "launch-test";
  request.request_digest_sha256 = std::string(64, '1');
  request.frames = frames;
  request.limits.max_frames = frames;
  request.limits.max_events = frames * 2;
  request.limits.max_bytes = 1024 * 1024;
  request.limits.max_line_bytes = 64 * 1024;
  request.limits.max_host_ms = 30000;
  request.limits.progress_interval_ms = 250;
  return request;
}

void exact_capture(std::uint64_t frames, bool completed_events) {
  EmucapRecordingRequest value = request(frames);
  value.include_frame_completed = completed_events;
  TestSink* sink_view = new TestSink();
  EmucapRecording recording(
      value, 100, 1000, std::unique_ptr<EmucapRecordingSink>(sink_view));
  assert(recording.tick(100, 1000) == EmucapRecordingEffect::none);
  for (std::uint64_t offset = 1; offset <= frames; ++offset) {
    const EmucapRecordingEffect effect = recording.tick(100 + offset, 1001 + offset);
    if (offset == frames) assert(effect == EmucapRecordingEffect::terminal);
    else assert(effect == EmucapRecordingEffect::none || effect == EmucapRecordingEffect::working);
  }
  assert(recording.progress_json().find("\"frames\":" + std::to_string(frames)) !=
         std::string::npos);
  const EmucapRecordingResult result = recording.result(2000);
  assert(!recording.active());
  assert(result.status == "completed");
  assert(result.operation_outcome == "completed");
  assert(result.execution_outcome == "target_reached");
  assert(result.integrity == "complete");
  assert(result.f_start == 100);
  assert(result.f_end == 100 + frames);
  assert(result.final_frame == 100 + frames);
  assert(result.frames == frames);
  assert(result.events == frames * (completed_events ? 2 : 1));
  assert(!result.input_acquired);
  assert(result.sink_released);
  assert(sink_view->closed());
  assert(sink_view->bytes().find("\"sequence\":0") != std::string::npos);
}

void event_stop_capture(std::uint64_t occurrence) {
  EmucapRecordingRequest value = request(3);
  value.include_frame_completed = true;
  value.stop_on_frame_completed = true;
  value.stop_occurrence = occurrence;
  EmucapRecording recording(
      value, 200, 1000,
      std::unique_ptr<EmucapRecordingSink>(new TestSink()));
  assert(recording.tick(200, 1000) == EmucapRecordingEffect::none);
  EmucapRecordingEffect effect = EmucapRecordingEffect::none;
  std::uint64_t boundary = 200;
  while (effect != EmucapRecordingEffect::terminal) {
    ++boundary;
    effect = recording.tick(boundary, 1000 + boundary - 200);
  }
  const EmucapRecordingResult result = recording.result(1100);
  assert(result.execution_outcome == "event_stop");
  assert(result.frames == occurrence);
  assert(result.events == occurrence * 2);
  assert(result.stop_occurrence == occurrence);
}

std::vector<std::string> movie_schedule(std::uint64_t host_step_ms) {
  EmucapRecordingRequest value = request(3);
  value.include_frame_completed = true;
  value.input_masks.push_back(1);
  value.input_masks.push_back(2);
  value.input_masks.push_back(0);
  std::vector<std::string> order;
  EmucapInputOverride input;
  EmucapRecording recording(
      value, 300, 100, std::unique_ptr<EmucapRecordingSink>(new TestSink(
          static_cast<std::size_t>(-1), 0, &order)),
      [&order, &input](bool engaged, std::uint16_t mask, std::string&) {
        order.push_back(engaged ? "input:" + std::to_string(mask) : "input:release");
        if (engaged) input.engage(mask);
        else input.release();
        return true;
      });
  assert(recording.tick(300, 100) == EmucapRecordingEffect::none);
  for (std::uint64_t offset = 1; offset <= 3; ++offset) {
    const EmucapRecordingEffect effect =
        recording.tick(300 + offset, 100 + host_step_ms * offset);
    if (offset == 3) assert(effect == EmucapRecordingEffect::terminal);
    else assert(effect == EmucapRecordingEffect::none ||
                effect == EmucapRecordingEffect::working);
  }
  const EmucapRecordingResult result = recording.result(100 + host_step_ms * 3);
  assert(result.status == "completed");
  assert(result.frames == 3 && result.events == 6);
  assert(result.input_acquired && result.input_released);
  assert(!input.engaged());

  unsigned char native_port[2] = {0xA5, 0x5A};
  input.apply(native_port, 2);
  assert(native_port[0] == 0xA5 && native_port[1] == 0x5A);
  return order;
}

int main() {
  assert(emucap_recording_capability_json(false).find(
             std::string("\"revision\":\"") + EMUCAP_RECORDING_CAPABILITY_REVISION) !=
         std::string::npos);
  assert(emucap_recording_capability_json(false).find("reset_release") == std::string::npos);
  assert(emucap_recording_capability_json(true).find(
             std::string("\"revision\":\"") + EMUCAP_RECORDING_RESET_CAPABILITY_REVISION) !=
         std::string::npos);
  assert(emucap_recording_capability_json(true).find("reset_release") != std::string::npos);
  assert(emucap_recording_capability_json(true).find("frame_completed") != std::string::npos);
  assert(emucap_recording_capability_json(true).find(EMUCAP_RECORDING_INPUT_MOVIE_FORMAT) !=
         std::string::npos);

  bool completed = true;
  const std::string boundary =
      std::string("{\"event_classes\":[{\"id\":\"frame_boundary\",\"contract_sha256\":\"") +
      EMUCAP_RECORDING_FRAME_CONTRACT_SHA256 + "\"}]}";
  assert(emucap_recording_exact_event_classes(boundary, completed));
  assert(!completed);
  const std::string both =
      boundary.substr(0, boundary.size() - 2) +
      ",{\"contract_sha256\":\"" + EMUCAP_RECORDING_COMPLETED_CONTRACT_SHA256 +
      "\",\"id\":\"frame_completed\"}]}";
  assert(emucap_recording_exact_event_classes(both, completed));
  assert(completed);
  assert(!emucap_recording_exact_event_classes(
      boundary.substr(0, boundary.size() - 2) +
          ",{\"id\":\"frame_boundary\",\"contract_sha256\":\"" +
          EMUCAP_RECORDING_FRAME_CONTRACT_SHA256 + "\"}]}",
      completed));

  {
    const EmucapRecordingButtonLookup lookup =
        [](const std::string& name, std::uint16_t& bit) {
          if (name == "a") bit = 1;
          else if (name == "b") bit = 2;
          else if (name == "confirm") bit = 1;
          else return false;
          return true;
        };
    std::vector<std::uint16_t> masks;
    std::string error;
    assert(emucap_parse_recording_input_movie(
        "0:a,b\n1:\n2:b\n", 3, lookup, masks, error));
    assert(masks.size() == 3 && masks[0] == 3 && masks[1] == 0 && masks[2] == 2);
    assert(!emucap_parse_recording_input_movie(
        "0:b,a\n1:\n", 2, lookup, masks, error));
    assert(!emucap_parse_recording_input_movie(
        "0:a,confirm\n1:\n", 2, lookup, masks, error));
    assert(!emucap_parse_recording_input_movie(
        "0:unknown\n1:\n", 2, lookup, masks, error));
    assert(!emucap_parse_recording_input_movie(
        "0:a\n2:b\n", 2, lookup, masks, error));
  }

  exact_capture(1, false);
  exact_capture(300, false);
  exact_capture(3, true);
  event_stop_capture(1);
  event_stop_capture(3);

  {
    const std::vector<std::string> fast = movie_schedule(1);
    const std::vector<std::string> delayed = movie_schedule(5000);
    const std::vector<std::string> expected = {
        "input:1", "event:boundary", "event:completed",
        "input:2", "event:boundary", "event:completed",
        "input:0", "event:boundary", "event:completed", "input:release"};
    assert(fast == expected);
    assert(delayed == expected);
  }

  {
    EmucapRecordingRequest value = request(3);
    value.include_frame_completed = true;
    value.stop_on_frame_completed = true;
    value.stop_occurrence = 2;
    value.input_masks.push_back(1);
    value.input_masks.push_back(2);
    value.input_masks.push_back(0);
    std::vector<std::string> order;
    std::vector<std::uint16_t> applied;
    TestSink* sink_view = new TestSink(
        static_cast<std::size_t>(-1), 0, &order);
    EmucapRecording recording(
        value, 10, 100, std::unique_ptr<EmucapRecordingSink>(sink_view),
        [&order, &applied](bool engaged, std::uint16_t mask, std::string&) {
          order.push_back(engaged ? "input:" + std::to_string(mask) : "input:release");
          if (engaged) applied.push_back(mask);
          return true;
        });
    assert(recording.tick(10, 100) == EmucapRecordingEffect::none);
    assert(recording.tick(11, 101) == EmucapRecordingEffect::none);
    assert(recording.tick(12, 102) == EmucapRecordingEffect::terminal);
    const EmucapRecordingResult result = recording.result(103);
    assert(result.status == "completed");
    assert(result.execution_outcome == "event_stop");
    assert(result.integrity == "complete");
    assert(result.frames == 2);
    assert(result.events == 4);
    assert(result.f_end == 12);
    assert(result.has_stop_event);
    assert(result.stop_sequence == 3);
    assert(result.stop_clock_tick == 12);
    assert(result.stop_frame == 11);
    assert(result.stop_occurrence == 2);
    assert(result.input_acquired && result.input_released);
    assert(applied.size() == 2 && applied[0] == 1 && applied[1] == 2);
    const std::vector<std::string> expected = {
        "input:1", "event:boundary", "event:completed", "input:2",
        "event:boundary", "event:completed", "input:release"};
    assert(order == expected);
  }

  {
    EmucapRecordingRequest value = request(3);
    value.input_masks.assign(3, 1);
    bool engaged = false;
    TestSink* sink_view = new TestSink(0);
    EmucapRecording recording(
        value, 10, 100, std::unique_ptr<EmucapRecordingSink>(sink_view),
        [&engaged](bool acquire, std::uint16_t, std::string&) {
          engaged = acquire;
          return true;
        });
    assert(recording.tick(10, 101) == EmucapRecordingEffect::terminal);
    const EmucapRecordingResult result = recording.result(102);
    assert(result.status == "failed");
    assert(result.integrity == "unverifiable");
    assert(result.events == 0);
    assert(result.input_acquired && result.input_released);
    assert(!engaged);
    assert(result.sink_released);
  }

  {
    EmucapRecording recording(
        request(3), 10, 100,
        std::unique_ptr<EmucapRecordingSink>(new TestSink(0, 7)));
    assert(recording.tick(10, 101) == EmucapRecordingEffect::terminal);
    const EmucapRecordingResult result = recording.result(102);
    assert(result.status == "failed");
    assert(result.integrity == "unverifiable");
    assert(result.events == 0);
    assert(result.physical_bytes == 7);
    assert(result.truncated);
  }

  {
    EmucapRecordingRequest value = request(3);
    value.include_frame_completed = true;
    TestSink* sink_view = new TestSink();
    EmucapRecording recording(
        value, 10, 100, std::unique_ptr<EmucapRecordingSink>(sink_view));
    assert(recording.tick(10, 100) == EmucapRecordingEffect::none);
    sink_view->fail_after_current();
    assert(recording.tick(11, 101) == EmucapRecordingEffect::terminal);
    const EmucapRecordingResult result = recording.result(102);
    assert(result.status == "failed");
    assert(result.frames == 1);
    assert(result.f_end == 11);
    assert(result.events == 1);
  }

  {
    EmucapRecording recording(
        request(3), 10, 100,
        std::unique_ptr<EmucapRecordingSink>(new TestSink()));
    assert(recording.tick(10, 100) == EmucapRecordingEffect::none);
    assert(recording.cancel(10, 101, "request_cancelled", "frozen") ==
           EmucapRecordingEffect::terminal);
    const EmucapRecordingResult result = recording.result(102);
    assert(result.status == "interrupted");
    assert(result.operation_outcome == "aborted");
    assert(result.integrity == "unverifiable");
    assert(result.frames == 0);
  }

  return 0;
}
