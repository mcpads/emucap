#!/usr/bin/env python3
"""Actual direct-adapter dispatch admission + batch handler at fixed frame.

Native load/reset effects are controlled at the handler seam, including a throw
following mutation. This verifies epoch ownership, not serializer correctness.
"""
from pathlib import Path
import ast,subprocess,tempfile
root=Path(__file__).resolve().parents[2]
tree=ast.parse((root/'_tests/native/direct-batch-preflight.py').read_text())
common=next(ast.literal_eval(n.value) for n in tree.body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id=='common' for t in n.targets))
def function(s,signature):
 start=s.index(signature);end=s.index('\n}',start)+2;return s[start:end]
support=r'''
#include <atomic>
bool g_input_control_unverified=false,g_pacing_control_unverified=false,g_recording=false;
bool g_renderer_unverified=false,g_failure_active=false;
std::atomic<bool> g_renderer_failure_pending{false};int g_step_remaining=0;
namespace EmucapControl {struct InputRequest{};}
void handle_abort_recording(long,const std::string&){}
bool exclude_renderer_writes(long){return true;}
bool failure_method_allowed(const std::string&){return true;}
std::string json_str(const std::string& s,const char* key){return json::parse(s).value(key,std::string());}
void json_num(const std::string& s,const char* key,long& value){value=json::parse(s).value(key,0L);}
bool fault=false;
'''
effect=r'''
 if(method=="read_memory_batch") {handle_read_memory_batch(id,line);return;}
 if(method=="save_state" || method=="execution_speed") return;
 // The seam stands for native replacement; it must observe the retired boundary.
 assert(g_boundary_seq>before_effect);memory[0]++;
 if(fault) throw std::runtime_error("partial effect");
'''
checks=r'''
int main(){
 u8 ram[16]={};memory=ram;std::string before;
 const auto batch=json({{"id",1},{"method","read_memory_batch"},{"params",{{"ranges",json::array({{{"memory_type","ram"},{"address",0},{"length",1}}})}}}}).dump();
 auto snapshot=[&]{dispatch(batch);assert(success);assert(reply["boundary"]["clocks"][0]["value"]==17);return reply["boundary"]["stop_epoch"].get<std::string>();};
 before=snapshot();assert(snapshot()==before);
 for(const char* method:{"load_state","reset","write_memory","probe"})for(bool failed:{false,true}){
  fault=failed;before_effect=g_boundary_seq;dispatch(json({{"id",1},{"method",method}}).dump());
  auto after=snapshot();assert(before!=after);assert(snapshot()==after);before=after;
 }
 for(const char* method:{"save_state","execution_speed"}){dispatch(json({{"id",1},{"method",method}}).dump());assert(snapshot()==before);}
}
'''
for adapter in ['mednafen','flycast']:
 s=(root/f'adapters/{adapter}/emucap.cpp').read_text()
 classify=function(s,'bool observation_method(');batch=function(s,'void handle_read_memory_batch(')
 if adapter=='mednafen':
  start=s.index('void handle_legacy(');end=s.index('  if (method == "hello")',start)
  admission=s[start:end]+effect+'\n } catch(const std::exception&) {}\n}\n'
  wrapper='void dispatch(const std::string& line){handle_legacy(1,json_str(line,"method"),line,{});}\n'
 else:
  start=s.index('void handle(const std::string& line) {');end=s.index('\t\tif (method == "hello")',start)
  admission=s[start:end]+effect+'\n } catch(const std::exception&) {}\n}\n'
  wrapper='void dispatch(const std::string& line){handle(line);}\n'
 code=common+support+'\nuint64_t before_effect=0;\n'+classify+batch+admission+wrapper+checks
 with tempfile.TemporaryDirectory() as d:
  p=Path(d)/'check.cpp';b=Path(d)/'check';p.write_text(code)
  subprocess.run(['clang++','-std=c++17','-O1','-fsanitize=address,undefined','-I'+str(root/f'adapters/{adapter}'),'-I'+str(root/'adapters/mednafen/work/mednafen/src/drivers'),str(p),'-o',str(b)],check=True)
  subprocess.run([str(b)],check=True)
 print(f'{adapter}: actual dispatch retires same-clock epoch before load/reset/write/probe effects and errors; batch/save/pacing preserve epoch')
