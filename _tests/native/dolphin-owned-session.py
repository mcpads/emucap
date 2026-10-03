#!/usr/bin/env python3
"""Run Dolphin's maintained owned session with real sockets and controlled native handlers."""
from pathlib import Path
import subprocess
import tempfile
root=Path(__file__).resolve().parents[2]
code=r'''
#include "EmuCapWire.h"
#include "EmuCapTemporal.h"
#include <picojson.h>
#include <atomic>
#include <cassert>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <fcntl.h>
#include <functional>
#include <future>
#include <mutex>
#include <sys/socket.h>
#include <unistd.h>
#include <thread>
using SOCKET=int;
namespace Temporal=EmuCap::Temporal;
using Json=Temporal::Json;
std::atomic<bool> s_stop{false},s_control_retired{false},s_request_cancelled{false},entered{false};
std::atomic<int> native_stops{0},state_runs{0};
std::mutex s_cancel_mutex,s_input_mutex;
std::optional<std::chrono::steady_clock::time_point> s_cancel_origin;
std::string s_handler_error,s_handler_error_kind;
unsigned s_boundary_seq=0;
struct Override {bool engaged=false;};
Override s_gamecube_input[4];
struct {bool engaged=false;uint16_t buttons=0;} s_wii_input;
std::string EnvOr(const char* field,const char*) {return std::string(field)=="EMUCAP_LAUNCH_ID"?"runtime":"gamecube";}
bool IsWiiSystem(const std::string&){return false;}
void ResetRequestCancellation(){std::lock_guard lock(s_cancel_mutex);s_cancel_origin.reset();s_request_cancelled=false;}
void CancelRequest(){std::lock_guard lock(s_cancel_mutex);if(!s_cancel_origin)s_cancel_origin=std::chrono::steady_clock::now();s_request_cancelled=true;}
namespace Core {
struct System{static System& GetInstance(){static System system;return system;}}; enum class State{Paused};
void CancelFrameStep(System&){++native_stops;}
State GetState(System&){return State::Paused;}
void QueueHostJob(std::function<void(System&)> job){static System system;job(system);}
}
struct SafeAccess{explicit SafeAccess(Core::System&) {}};
bool memory_park_verified=true;
bool AwaitMemoryPark(Core::System&,std::chrono::steady_clock::time_point){return memory_park_verified;}
bool VerifyFrozenPublication(Core::System&,const picojson::object&,bool){return true;}
using Handler=picojson::object(*)(Core::System&,const picojson::object&);
bool ObservationMethod(const std::string& method){return method=="hello"||method=="status"||method=="save_state";}
picojson::object Hello(Core::System&,const picojson::object&){return {{"methods",picojson::value(picojson::array{})}};}
picojson::object Input(Core::System&,const picojson::object&){std::lock_guard lock(s_input_mutex);s_gamecube_input[0].engaged=true;return {};}
picojson::object Step(Core::System&,const picojson::object&){entered=true;while(!s_request_cancelled)std::this_thread::sleep_for(std::chrono::milliseconds(1));return {{"state",picojson::value(std::string("frozen"))}};}
Handler Lookup(const std::string& method){if(method=="hello"||method=="status"||method=="save_state")return Hello;if(method=="set_input")return Input;if(method=="step")return Step;return nullptr;}
class StateCall {
public:
 picojson::object Run(Core::System& system,Handler h,picojson::object p){++state_runs;return h(system,p);}
};
''' + (root/'adapters/dolphin/EmuCapOwned.inl').read_text() + r'''
struct Connection {
 int fd[2]; Core::System system;std::thread server;std::string pending;
 Connection(){assert(socketpair(AF_UNIX,SOCK_STREAM,0,fd)==0);
#ifdef __APPLE__
 int one=1;setsockopt(fd[0],SOL_SOCKET,SO_NOSIGPIPE,&one,sizeof(one));
#endif
 server=std::thread([this]{ServeOwnedSession(system,fd[0]);close(fd[0]);});}
 void Send(const Json& v){assert(OwnedWrite(fd[1],v));}
 Json Reply(uint64_t id){for(;;){Json value;auto r=ReadOwned(fd[1],pending,value);assert(r!=OwnedRead::Closed);if(r==OwnedRead::Ready){assert(value["id"]==id);if(value.contains("result")&&value["result"].value("status",std::string{})=="working")continue;return value;}}}
 Json Call(uint64_t id,const std::string& method,Json params=Json::object()){Send({{"v",1},{"id",id},{"method",method},{"params",params}});return Reply(id);}
 void Close(){close(fd[1]);server.join();}
};
Json key(std::string op){return {{"runtime","runtime"},{"owner_id","owner"},{"operation_id",op}};}
bool held(){std::lock_guard lock(s_input_mutex);return s_gamecube_input[0].engaged;}
int main(){
 {Connection c;
 assert(c.Call(1,"hello")["result"]["control_session_lifecycle"]==true);
 assert(c.Call(2,"begin_temporal_operation",{{"parent",key("parent")}})["ok"]==true);
 assert(c.Call(3,"set_input",{{"_temporal_owner",key("parent")},{"buttons",{"a"}}})["ok"]==true);assert(held());
 assert(c.Call(4,"save_state",{{"_temporal_owner",key("parent")}})["ok"]==false);assert(state_runs==0);
 c.Send({{"v",1},{"id",5},{"method","step"},{"params",{{"_temporal_owner",key("parent")},{"_control",key("child")},{"frames",120}}}});
 while(!entered)std::this_thread::yield();
 assert(c.Call(6,"cancel_operation",key("foreign"))["result"]["status"]=="not_active");assert(!s_request_cancelled);
 c.Send({{"v",1},{"id",7},{"method","cancel_operation"},{"params",key("child")}});
 assert(c.Reply(7)["result"]["status"]=="requested");assert(c.Reply(5)["ok"]==true);assert(held());
 auto done=c.Call(8,"finish_temporal_operation",{{"parent",key("parent")}});assert(done["result"]["cleanup_verified"]==true&&!held());
 int stops=native_stops;assert(c.Call(9,"finish_temporal_operation",{{"parent",key("parent")}})["result"]==done["result"]);assert(native_stops==stops);
 assert(c.Call(10,"save_state")["ok"]==true);
 c.Close();assert(state_runs==1);
 }
 // An EOF between input and child execution still owns and releases that port.
 {Connection c;assert(c.Call(1,"begin_temporal_operation",{{"parent",key("next")}})["ok"]==true);
 assert(c.Call(2,"set_input",{{"_temporal_owner",key("next")}})["ok"]==true);assert(held());c.Close();assert(!held());}
 // An unscoped persistent hold has no parent obligation.
 {Connection c;assert(c.Call(1,"set_input")["ok"]==true);c.Close();assert(held());}
 // Broker lifecycle uses full registration/session identities and ordered detach.
 {Connection c;
 Json attachment={{"broker_instance","broker"},{"registration",UINT64_MAX},{"session",uint64_t{1}}};
 auto event=[&](bool attach,const Json& stamp){c.Send({{"_control_session",{{"kind",attach?"attach":"detach"},{"runtime","runtime"},{"attachment",stamp}}}});};
 event(true,attachment);
 assert(c.Call(1,"begin_temporal_operation",{{"parent",key("broker-parent")},{"_temporal_owner",key("broker-parent")},{"_control_attachment",attachment}})["ok"]==true);
 assert(c.Call(2,"set_input",{{"_temporal_owner",key("broker-parent")},{"_control_attachment",attachment}})["ok"]==true);
 entered=false;
 c.Send({{"v",1},{"id",3},{"method","step"},{"params",{{"_temporal_owner",key("broker-parent")},{"_control",key("broker-child")},{"_control_attachment",attachment},{"frames",120}}}});
 while(!entered)std::this_thread::yield();
 Json replacement=attachment;replacement["session"]=uint64_t{2};
 event(false,replacement); // stale detach cannot cancel this child
 assert(c.Call(4,"cancel_operation",{{"runtime","runtime"},{"owner_id","owner"},{"operation_id","foreign"},{"_control_attachment",attachment}})["result"]["status"]=="not_active");
 assert(!s_request_cancelled && held());
 event(false,attachment);event(true,replacement);
 assert(c.Reply(3)["ok"]==true);
 assert(c.Call(5,"status",{{"_control_attachment",replacement}})["ok"]==true);
 assert(!held());
 assert(c.Call(6,"finish_temporal_operation",{{"parent",key("broker-parent")},{"_control_attachment",replacement}})["ok"]==false);
 c.Close();}
 assert(!s_control_retired);
 // Releasing inputs and stopping the CPU cannot certify a pending guest writer.
 {Connection c;
 assert(c.Call(1,"begin_temporal_operation",{{"parent",key("pending-writer")}})["ok"]==true);
 assert(c.Call(2,"set_input",{{"_temporal_owner",key("pending-writer")}})["ok"]==true);
 memory_park_verified=false;
 assert(c.Call(3,"finish_temporal_operation",{{"parent",key("pending-writer")}})["ok"]==false);
 c.Close();assert(s_control_retired);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='dolphin-session-') as directory:
 p=Path(directory);(p/'test.cpp').write_text(code)
 subprocess.run(['c++','-std=c++20','-fno-exceptions','-pthread','-Wall','-Wextra','-Werror',
  '-I'+str(root/'adapters/dolphin'),'-isystem',str(root/'adapters/dolphin/work/dolphin-src/Externals/tinygltf/tinygltf'),
  '-isystem',str(root/'adapters/dolphin/work/dolphin-src/Externals/picojson'),str(p/'test.cpp'),'-o',str(p/'test')],check=True)
 subprocess.run([str(p/'test')],check=True,timeout=20)
print('Dolphin owned session: stale cancel, scoped cleanup, retained finish, idle EOF and state ownership passed')
