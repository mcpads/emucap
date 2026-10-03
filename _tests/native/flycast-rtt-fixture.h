// Controlled GL and conversion dependencies for the actual native RTT callback.
// This checks writer lifetime, not GPU rendering or pixel conversion accuracy.
#include <cstdint>
#include <vector>
#include <memory>
using u8=uint8_t; using u16=uint16_t; using u32=uint32_t;
using GLint=int; using GLuint=unsigned;
constexpr int GL_PACK_ALIGNMENT=1, GL_IMPLEMENTATION_COLOR_READ_FORMAT=2,
 GL_IMPLEMENTATION_COLOR_READ_TYPE=3, GL_RGB=4, GL_UNSIGNED_SHORT_5_6_5=5,
 GL_RGBA=6, GL_UNSIGNED_BYTE=7, GL_FRAMEBUFFER=8;
constexpr unsigned VRAM_MASK=255;
alignas(4) u8 vram[256]{};
struct Control {u8 fb_packmode=1;};
struct Rect {struct Pair {int x,y;}; Pair origin{0,0},size{4,4};};
struct Context {u32 framebufferWidth=4,framebufferHeight=4;Control fb_W_CTRL;
 u32 fb_W_SOF1=0,fb_W_LINESTRIDE=1;Rect fbClip;};
Context context;
struct Framebuffer {GLuint detachTexture(){return 0;}};
struct {Context* rendContext=&context;
 struct {std::unique_ptr<Framebuffer> framebuffer=std::make_unique<Framebuffer>();} rtt;
 struct {GLuint origFbo=0;} ofbo;} gl;
namespace config {bool RenderToTextureBuffer=true;}
template<typename T> struct PixelBuffer {std::vector<T> values;
 void init(u32 w,u32 h){values.resize(w*h);}T* data(){return values.data();}};
struct TextureCacheData {GLuint texID=0;int dirty=1;void unprotectVRam(){}};
struct {TextureCacheData texture;TextureCacheData* getRTTexture(u32,u8,int,int){return &texture;}} TexCache;
struct {void DeleteTextures(int,GLuint*){}} glcache;
void getPvrFramebufferSize(Context&,int& w,int& h){w=h=4;}
void glPixelStorei(int,int){}
void glGetIntegerv(int name,GLint* value){*value=name==GL_IMPLEMENTATION_COLOR_READ_FORMAT?GL_RGB:GL_UNSIGNED_SHORT_5_6_5;}
void glCheck(){}
void glBindFramebuffer(int,GLuint){}
std::function<void(void*,bool)> controlled_read;
std::function<void()> controlled_convert;
void glReadPixels(int,int,u32,u32,int,int type,void* data){controlled_read(data,type==GL_UNSIGNED_SHORT_5_6_5);}
void WriteTextureToVRam(u32,u32,u8*,u16* dst,Control,u32,Rect){
 assert(dst==reinterpret_cast<u16*>(vram));
 dst[0]=11;controlled_convert();dst[15]=22;
}
// ACTUAL_RTT_CALLBACK
void check_rtt_writer(){
 for(bool direct : {true,false}) {
  std::fill(std::begin(vram),std::end(vram),0);
  context.fb_W_CTRL.fb_packmode=direct?1:0;
  std::promise<void> entered,release;auto gate=release.get_future();
  controlled_read=[&](void* data,bool native_direct){
   assert(native_direct==direct);
   if(direct){assert(data==vram);auto dst=static_cast<u16*>(data);
    dst[0]=11;entered.set_value();gate.wait();dst[15]=22;}
   else {assert(data!=vram);}
  };
  controlled_convert=[&]{entered.set_value();gate.wait();};
  PvrMessageQueue q;writer=[]{ReadRTTBuffer();};q.enqueue(PvrMessageQueue::Render);
  auto consumer=std::async(std::launch::async,[&]{return q.waitAndExecute();});
  entered.get_future().wait();auto before=signals.load();
  auto observer=std::async(std::launch::async,[&]{return q.waitForWriters(2000);});
  await_submission(before);
  assert(observer.wait_for(10ms)==std::future_status::timeout);
  release.set_value();assert(consumer.get());assert(q.waitAndExecute(0));
  assert(observer.get()==Result::complete);
  auto dst=reinterpret_cast<u16*>(vram);assert(dst[0]==11 && dst[15]==22);
 }
}
