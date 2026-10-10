#include "bootstrap.h"
#import "permissions.h"
#import <CommonCrypto/CommonDigest.h>
#include <sys/stat.h>
#include <node_api.h>
#include <cmath>
#include <cstring>
#include <set>
#include <stdexcept>
#include <vector>

namespace {
constexpr napi_type_tag kReceiverTag{0xb15c3833c16a4133ULL, 0x9a89a9c839de91b6ULL};
void Check(napi_status status) {
  if (status != napi_ok) throw std::runtime_error("Native PiP Node-API operation failed");
}
void Require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
napi_value Get(napi_env env, napi_value object, const char* key) {
  napi_value value; Check(napi_get_named_property(env, object, key, &value)); return value;
}
std::string String(napi_env env, napi_value value) {
  napi_valuetype type; Check(napi_typeof(env, value, &type));
  Require(type == napi_string, "PiP identity must be a string");
  size_t length; Check(napi_get_value_string_utf8(env, value, nullptr, 0, &length));
  Require(length && length <= octopus::pip::kMaxMetadataBytes, "PiP identity exceeds its bound");
  std::vector<char> buffer(length + 1);
  Check(napi_get_value_string_utf8(env, value, buffer.data(), buffer.size(), &length));
  std::string text(buffer.data(), length);
  Require(text.find('\0') == std::string::npos, "PiP identity contains a null byte");
  return text;
}
double Number(napi_env env, napi_value value) {
  double number; Check(napi_get_value_double(env, value, &number));
  Require(std::isfinite(number), "PiP geometry must be finite"); return number;
}
uint64_t Sequence(napi_env env, napi_value value) {
  const double number = Number(env, value);
  Require(number > 0 && number <= octopus::pip::kMaxSequence && std::floor(number) == number,
          "PiP generation must be an exact positive safe integer");
  return static_cast<uint64_t>(number);
}
bool Boolean(napi_env env, napi_value value) {
  bool result; Check(napi_get_value_bool(env, value, &result)); return result;
}
void Keys(napi_env env, napi_value object, const std::set<std::string>& expected) {
  napi_valuetype type; Check(napi_typeof(env, object, &type));
  bool array = false; Check(napi_is_array(env, object, &array));
  Require(type == napi_object && !array, "PiP request must be an object");
  napi_value names; Check(napi_get_all_property_names(env, object, napi_key_own_only,
      napi_key_all_properties, napi_key_numbers_to_strings, &names));
  uint32_t count; Check(napi_get_array_length(env, names, &count));
  Require(count == expected.size(), "PiP request has missing or extra fields");
  for (uint32_t i = 0; i < count; ++i) {
    napi_value name; Check(napi_get_element(env, names, i, &name));
    Require(expected.contains(String(env, name)), "PiP request has an unknown field");
  }
}
CGRect Rect(napi_env env, napi_value object) {
  Keys(env, object, {"x", "y", "width", "height"});
  return CGRectMake(Number(env, Get(env, object, "x")), Number(env, Get(env, object, "y")),
                    Number(env, Get(env, object, "width")), Number(env, Get(env, object, "height")));
}
napi_value Text(napi_env env, const std::string& text) {
  napi_value value; Check(napi_create_string_utf8(env, text.data(), text.size(), &value)); return value;
}
void Set(napi_env env, napi_value object, const char* key, napi_value value) {
  Check(napi_set_named_property(env, object, key, value));
}
napi_value Numeric(napi_env env, double number) {
  napi_value value; Check(napi_create_double(env, number, &value)); return value;
}
napi_value Bool(napi_env env, bool boolean) {
  napi_value value; Check(napi_get_boolean(env, boolean, &value)); return value;
}

struct Binding {
  napi_threadsafe_function events = nullptr;
  std::shared_ptr<octopus::pip::ReceiverBootstrap> host;
  NSMutableDictionary<NSString*,OCTPermissionWindowController*>* __strong permissions = [NSMutableDictionary dictionary];
  NSBundle* __strong settingsResources;
  bool closing = false;
  ~Binding() { Close(); }
  void Close() {
    if (closing) return; closing = true;
    for(OCTPermissionWindowController* controller in permissions.allValues)[controller invalidate];
    [permissions removeAllObjects];
    if (host) host->Stop();
    if (events) { napi_release_threadsafe_function(events,napi_tsfn_abort); events = nullptr; }
  }
};
struct Environment { std::weak_ptr<Binding> binding; };
struct EventPayload { NSDictionary* __strong value; };

id FoundationValue(napi_env env, napi_value value, size_t depth = 0) {
  Require(depth <= 16, "PiP value nesting exceeds its bound"); napi_valuetype type; Check(napi_typeof(env,value,&type));
  if (type == napi_null) return NSNull.null;
  if (type == napi_boolean) return @(Boolean(env,value));
  if (type == napi_number) return @(Number(env,value));
  if (type == napi_string) {
    size_t length; Check(napi_get_value_string_utf8(env,value,nullptr,0,&length)); Require(length <= 65536,"PiP text exceeds its bound");
    std::vector<char> buffer(length+1); Check(napi_get_value_string_utf8(env,value,buffer.data(),buffer.size(),&length));
    NSString* text = [[NSString alloc] initWithBytes:buffer.data() length:length encoding:NSUTF8StringEncoding]; Require(text != nil,"PiP text is invalid"); return text;
  }
  Require(type == napi_object,"PiP value must be plain JSON data"); bool array; Check(napi_is_array(env,value,&array));
  if (array) {
    uint32_t count; Check(napi_get_array_length(env,value,&count)); Require(count <= 1024,"PiP collection exceeds its bound"); NSMutableArray* result = [NSMutableArray arrayWithCapacity:count];
    for (uint32_t i = 0; i < count; ++i) { napi_value item; Check(napi_get_element(env,value,i,&item)); [result addObject:FoundationValue(env,item,depth+1)]; } return result;
  }
  napi_value names; Check(napi_get_all_property_names(env,value,napi_key_own_only,napi_key_all_properties,napi_key_numbers_to_strings,&names));
  uint32_t count; Check(napi_get_array_length(env,names,&count)); Require(count <= 1024,"PiP object exceeds its bound"); NSMutableDictionary* result = [NSMutableDictionary dictionaryWithCapacity:count];
  for (uint32_t i = 0; i < count; ++i) {
    napi_value name,item; Check(napi_get_element(env,names,i,&name)); auto key = String(env,name); Check(napi_get_property(env,value,name,&item));
    result[[NSString stringWithUTF8String:key.c_str()]] = FoundationValue(env,item,depth+1);
  }
  return result;
}
napi_value JSValue(napi_env env, id value, size_t depth = 0) {
  Require(depth <= 16,"PiP event nesting exceeds its bound"); napi_value result;
  if (!value || value == NSNull.null) { Check(napi_get_null(env,&result)); return result; }
  if ([value isKindOfClass:NSString.class]) {
    NSData* data = [value dataUsingEncoding:NSUTF8StringEncoding]; Require(data.length <= 65536,"PiP event text exceeds its bound");
    Check(napi_create_string_utf8(env,static_cast<const char*>(data.bytes),data.length,&result)); return result;
  }
  if ([value isKindOfClass:NSNumber.class]) {
    if (CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return Bool(env,[value boolValue]);
    Require(std::isfinite([value doubleValue]),"PiP event number is invalid"); return Numeric(env,[value doubleValue]);
  }
  if ([value isKindOfClass:NSArray.class]) {
    Require([value count] <= 1024,"PiP event collection exceeds its bound"); Check(napi_create_array_with_length(env,[value count],&result));
    for (NSUInteger i = 0; i < [value count]; ++i) Check(napi_set_element(env,result,i,JSValue(env,value[i],depth+1))); return result;
  }
  Require([value isKindOfClass:NSDictionary.class] && [value count] <= 1024,"PiP events may contain metadata only"); Check(napi_create_object(env,&result));
  for (NSString* key in value) { Require([key isKindOfClass:NSString.class],"PiP event key is invalid"); Check(napi_set_named_property(env,result,key.UTF8String,JSValue(env,value[key],depth+1))); }
  return result;
}
void CallEvent(napi_env env, napi_value callback, void*, void* raw) {
  std::unique_ptr<EventPayload> event(static_cast<EventPayload*>(raw)); if (!env || !callback) return;
  napi_handle_scope scope = nullptr;
  try {
    Check(napi_open_handle_scope(env,&scope)); napi_value object = JSValue(env,event->value), global,result; Check(napi_get_global(env,&global));
    auto status = napi_call_function(env,global,callback,1,&object,&result);
    if (status == napi_pending_exception) { napi_value exception; Check(napi_get_and_clear_last_exception(env,&exception)); Check(napi_fatal_exception(env,exception)); }
    else Check(status);
  } catch (const std::exception& error) {
    napi_value message,exception;
    if (napi_create_string_utf8(env,error.what(),NAPI_AUTO_LENGTH,&message) == napi_ok && napi_create_error(env,nullptr,message,&exception) == napi_ok) napi_fatal_exception(env,exception);
    else napi_fatal_error("OctopusNativePIP",NAPI_AUTO_LENGTH,"PiP async metadata delivery failed",NAPI_AUTO_LENGTH);
  }
  if (scope) napi_close_handle_scope(env,scope);
}
void Emit(const std::weak_ptr<Binding>& weak, NSDictionary* event) {
  auto binding = weak.lock(); if (!binding || binding->closing || !binding->events) return;
  auto payload = std::make_unique<EventPayload>(); payload->value = [event copy];
  if (napi_call_threadsafe_function(binding->events,payload.get(),napi_tsfn_nonblocking) == napi_ok) { payload.release(); return; }
  binding->Close(); throw std::runtime_error("PiP async event queue is unavailable; native host closed");
}
NSString* NSStringValue(napi_env env, napi_value value) { return [NSString stringWithUTF8String:String(env,value).c_str()]; }
NSData* Bytes(napi_env env, napi_value value, size_t maximum, void** pointer = nullptr) {
  bool buffer; Check(napi_is_buffer(env,value,&buffer)); Require(buffer,"PiP private native input requires a Buffer");
  void* bytes; size_t size; Check(napi_get_buffer_info(env,value,&bytes,&size)); Require(size && size <= maximum,"PiP private native input exceeds its bound");
  if (pointer) { *pointer = bytes; return [NSData dataWithBytesNoCopy:bytes length:size freeWhenDone:NO]; }
  return [NSData dataWithBytes:bytes length:size];
}
struct Wipe {
  void* pointer; size_t length;
  ~Wipe() { volatile uint8_t* bytes = static_cast<volatile uint8_t*>(pointer); for (size_t i=0;i<length;++i) bytes[i]=0; }
};

enum class Method { PrepareAutomation, BeginAutomation, CancelAutomationSettings, ShowPermissions, UpdatePermissions, UpdateSettings, ClosePermissions, RegisterHost, RemoveHost, BeginSource, InstallSource, CancelSource, RevokeSource, UpsertBrowserImage, InvalidateBrowserImage, CompleteGroup, ClearCompletedGroup, Selection, Placement, MoveToHost, MaxDisplaySize, Status, Stop };
template<Method method> napi_value Call(napi_env env,napi_callback_info info) {
  try {
    Require([NSThread isMainThread],"PiP host must run in Electron main"); size_t argc=2; napi_value args[2],self; void* data;
    Check(napi_get_cb_info(env,info,&argc,args,&self,&data)); bool tagged=false; Check(napi_check_object_type_tag(env,self,&kReceiverTag,&tagged)); Require(tagged,"PiP method requires its opaque native host");
    void* raw; Check(napi_unwrap(env,self,&raw)); auto binding=*static_cast<std::shared_ptr<Binding>*>(raw); Require(binding && binding->host,"PiP native host is unavailable");
    auto& host=*binding->host;
    if constexpr (method == Method::Status) { Require(argc==0,"PiP status takes no arguments"); return JSValue(env,host.Status()); }
    else if constexpr (method == Method::Stop) { Require(argc==0,"PiP stop takes no arguments"); binding->Close(); }
    else {
      Require(!binding->closing && argc==1,"PiP mutation requires one request and a live native host"); napi_value input=args[0];
      if constexpr (method == Method::PrepareAutomation) {
        Keys(env,input,{"requestId"});return JSValue(env,host.PrepareAutomationPermission(NSStringValue(env,Get(env,input,"requestId"))));
      } else if constexpr (method == Method::BeginAutomation) {
        Keys(env,input,{"requestId","operationId","operation","timeoutMs"});host.BeginAutomationPermission(NSStringValue(env,Get(env,input,"requestId")),NSStringValue(env,Get(env,input,"operationId")),NSStringValue(env,Get(env,input,"operation")),Sequence(env,Get(env,input,"timeoutMs")));
      } else if constexpr (method == Method::CancelAutomationSettings) {
        Keys(env,input,{"requestId","operationId"});host.CancelAutomationSettings(NSStringValue(env,Get(env,input,"requestId")),NSStringValue(env,Get(env,input,"operationId")));
      } else if constexpr (method == Method::ShowPermissions) {
        Keys(env,input,{"presentationId","generation","windowHandle","snapshot"});
        NSString* identifier=NSStringValue(env,Get(env,input,"presentationId"));
        Require(!binding->permissions[identifier] && binding->permissions.count<64,"Permission presentation already exists or its bound was reached");
        NSData* bytes=Bytes(env,Get(env,input,"windowHandle"),sizeof(void*));Require(bytes.length==sizeof(void*),"Permission window handle has the wrong native size");
        const void* handle=nullptr;std::memcpy(&handle,bytes.bytes,sizeof(handle));NSWindow* owner=nil;
        for(NSWindow* window in NSApp.windows)if((__bridge const void*)window.contentView==handle){owner=window;break;}
        Require(owner!=nil,"Permission owner is not an actual current client window");
        NSError* failure=nil;auto weak=std::weak_ptr<Binding>(binding);
        OCTPermissionWindowController* controller=[[OCTPermissionWindowController alloc] initWithOwner:owner identifier:identifier
          generation:NSStringValue(env,Get(env,input,"generation")) snapshot:FoundationValue(env,Get(env,input,"snapshot")) resourceBundle:binding->settingsResources
          event:^(NSDictionary* event){Emit(weak,event);} error:&failure];
        Require(controller!=nil && !failure,"Native permission window could not be created");binding->permissions[identifier]=controller;[controller present];
      } else if constexpr(method == Method::UpdatePermissions) {
        Keys(env,input,{"presentationId","generation","sequence","snapshot","error"});
        NSString* identifier=NSStringValue(env,Get(env,input,"presentationId"));OCTPermissionWindowController* controller=binding->permissions[identifier];
        Require(controller!=nil && [controller hasGeneration:NSStringValue(env,Get(env,input,"generation"))],"Permission presentation generation is unavailable");
        id failureText=FoundationValue(env,Get(env,input,"error"));Require(failureText==NSNull.null||[failureText isKindOfClass:NSString.class],"Permission error must be text");
        NSError* failure=nil;Require([controller update:FoundationValue(env,Get(env,input,"snapshot")) sequence:Sequence(env,Get(env,input,"sequence"))
          errorText:failureText==NSNull.null?nil:failureText error:&failure]&&!failure,"Permission response is stale or changed its helper");
      } else if constexpr(method == Method::UpdateSettings) {
        Keys(env,input,{"presentationId","generation","sequence","source"});
        auto controller=binding->permissions[NSStringValue(env,Get(env,input,"presentationId"))];
        Require(controller && [controller hasGeneration:NSStringValue(env,Get(env,input,"generation"))],"Settings accessory presentation owner changed");
        id source=FoundationValue(env,Get(env,input,"source"));Require(source==NSNull.null || [source isKindOfClass:NSDictionary.class],"Settings accessory source must be a private witness");
        NSError* failure=nil;Require([controller updateSettingsSource:source==NSNull.null?nil:source sequence:Sequence(env,Get(env,input,"sequence")) error:&failure]&&!failure,"Settings accessory source changed or is no longer pending");
      } else if constexpr(method == Method::ClosePermissions) {
        Keys(env,input,{"presentationId","generation"});NSString* identifier=NSStringValue(env,Get(env,input,"presentationId"));
        OCTPermissionWindowController* controller=binding->permissions[identifier];Require(!controller||[controller hasGeneration:NSStringValue(env,Get(env,input,"generation"))],"Permission presentation generation differs");
        [controller invalidate];[binding->permissions removeObjectForKey:identifier];
      } else if constexpr (method == Method::BeginSource) {
        Keys(env,input,{"requestId","scope","source","sourceWindowId","groupId","batchId","timeoutMs"}); return JSValue(env,host.BeginSource(FoundationValue(env,input)));
      } else if constexpr (method == Method::InstallSource) {
        Keys(env,input,{"requestId","bindingBytes","key"}); void* pointer=nullptr; NSData* key=Bytes(env,Get(env,input,"key"),32,&pointer); Wipe wipe{pointer,key.length}; Require(key.length==32,"PiP private key must have exactly32 bytes");
        host.InstallSource(NSStringValue(env,Get(env,input,"requestId")),Bytes(env,Get(env,input,"bindingBytes"),65536),key);
      } else if constexpr (method == Method::CancelSource) {
        Keys(env,input,{"requestId"}); host.CancelSource(NSStringValue(env,Get(env,input,"requestId")));
      } else if constexpr (method == Method::RevokeSource) {
        Keys(env,input,{"scope","sourceWindowId"}); napi_value window=Get(env,input,"sourceWindowId"); napi_valuetype type; Check(napi_typeof(env,window,&type));
        host.RevokeSource(FoundationValue(env,Get(env,input,"scope")),type==napi_null ? nil : NSStringValue(env,window));
      } else if constexpr (method == Method::UpsertBrowserImage) {
        Keys(env,input,{"identity","groupId","batchId","sequence","image"});
        host.UpsertBrowserImage(FoundationValue(env,Get(env,input,"identity")),NSStringValue(env,Get(env,input,"groupId")),
          NSStringValue(env,Get(env,input,"batchId")),Sequence(env,Get(env,input,"sequence")),Bytes(env,Get(env,input,"image"),100000000));
      } else if constexpr (method == Method::InvalidateBrowserImage) {
        Keys(env,input,{"identity"}); host.InvalidateBrowserImage(FoundationValue(env,Get(env,input,"identity")));
      } else if constexpr (method == Method::CompleteGroup || method == Method::ClearCompletedGroup) {
        Keys(env,input,{"groupId"});
        if constexpr(method == Method::CompleteGroup) host.CompleteGroup(NSStringValue(env,Get(env,input,"groupId")));
        else host.ClearCompletedGroup(NSStringValue(env,Get(env,input,"groupId")));
      } else if constexpr (method == Method::RegisterHost) {
        Keys(env,input,{"hostId","generation","windowHandle","anchorContentRect","passthroughRect","anchors","scope","role","homeAvailable","animated"});
        NSData* bytes=Bytes(env,Get(env,input,"windowHandle"),sizeof(void*)); Require(bytes.length==sizeof(void*),"PiP window handle has the wrong native pointer size"); const void* handle=nullptr; std::memcpy(&handle,bytes.bytes,sizeof(handle));
        napi_value rows=Get(env,input,"anchors"); bool array; uint32_t count; Check(napi_is_array(env,rows,&array)); Require(array,"PiP anchors must be an array"); Check(napi_get_array_length(env,rows,&count)); Require(count<=1024,"PiP anchors exceed their bound");
        NSMutableArray* anchors=[NSMutableArray array];
        for(uint32_t i=0;i<count;++i){napi_value row;Check(napi_get_element(env,rows,i,&row));Keys(env,row,{"x","y","alignment"});double alignment=Number(env,Get(env,row,"alignment"));Require(alignment>=0&&alignment<=5&&std::floor(alignment)==alignment,"PiP alignment is invalid");[anchors addObject:[[OCTPIPStackHostAnchor alloc] initWithPoint:CGPointMake(Number(env,Get(env,row,"x")),Number(env,Get(env,row,"y"))) alignment:static_cast<OCTPIPStackAlignment>(alignment)]];}
        auto scope=String(env,Get(env,input,"scope")),role=String(env,Get(env,input,"role"));Require(scope=="active"||scope=="all","PiP host scope is invalid");Require(role=="main"||role=="accessory","PiP host role is invalid");
        host.Hosts().RegisterHost(String(env,Get(env,input,"hostId")),String(env,Get(env,input,"generation")),handle,Rect(env,Get(env,input,"anchorContentRect")),Rect(env,Get(env,input,"passthroughRect")),anchors,scope=="all"?OCTPIPStackAll:OCTPIPStackActive,role=="main"?OCTPIPStackMain:OCTPIPStackAccessory,Boolean(env,Get(env,input,"homeAvailable")),Boolean(env,Get(env,input,"animated")));
      } else if constexpr (method == Method::RemoveHost) {
        Keys(env,input,{"hostId","generation"});host.Hosts().RemoveHost(String(env,Get(env,input,"hostId")),String(env,Get(env,input,"generation")));
      } else if constexpr (method == Method::Selection) {
        Keys(env,input,{"activeGroupId","suppressedGroupIds"});id group=FoundationValue(env,Get(env,input,"activeGroupId"));id suppressed=FoundationValue(env,Get(env,input,"suppressedGroupIds"));
        Require(group==NSNull.null||[group isKindOfClass:NSString.class],"PiP active group is invalid");Require([suppressed isKindOfClass:NSArray.class],"PiP suppressed groups must be an array");for(id value in suppressed)Require([value isKindOfClass:NSString.class],"PiP suppressed group is invalid");
        [host.Stack() setActiveGroup:group==NSNull.null?nil:group suppressedGroups:[NSSet setWithArray:suppressed]];
      } else if constexpr (method == Method::Placement || method == Method::MoveToHost) {
        if constexpr(method==Method::Placement)Keys(env,input,{"alignment","floating"});else Keys(env,input,{"alignment","hostId","generation"});
        double alignment=Number(env,Get(env,input,"alignment"));Require(alignment>=0&&alignment<=5&&std::floor(alignment)==alignment,"PiP alignment is invalid");
        if constexpr(method==Method::Placement)[host.Stack() setPreferredAlignment:static_cast<OCTPIPStackAlignment>(alignment) floating:Boolean(env,Get(env,input,"floating"))];
        else {NSError* error=nil;Require([host.Stack() moveToHost:NSStringValue(env,Get(env,input,"hostId")) generation:NSStringValue(env,Get(env,input,"generation")) alignment:static_cast<OCTPIPStackAlignment>(alignment) error:&error]&&!error,"PiP native host move was rejected");}
      } else if constexpr (method == Method::MaxDisplaySize) {
        Keys(env,input,{"size"});NSError* error=nil;Require([host.Stack() setMaxDisplaySize:Number(env,Get(env,input,"size")) error:&error]&&!error,"PiP maximum display size was rejected");
      }
    }
    napi_value result;Check(napi_get_undefined(env,&result));return result;
  }catch(const std::exception& error){napi_throw_error(env,nullptr,error.what());return nullptr;}
}

napi_value Create(napi_env env,napi_callback_info info) {
  try {
    Require([NSThread isMainThread],"PiP host must run in Electron main");napi_value global;Check(napi_get_global(env,&global));Require(String(env,Get(env,Get(env,global,"process"),"type"))=="browser","PiP host is only exposed to Electron main");
    size_t argc=2;napi_value args[2];void* data;Check(napi_get_cb_info(env,info,&argc,args,nullptr,&data));Require(argc==1,"PiP host requires one configuration");
    Keys(env,args[0],{"controlText","placementAssetPath","settingsAssetBundlePath","onEvent"});auto* environment=static_cast<Environment*>(data);auto previous=environment->binding.lock();Require(!previous||previous->closing,"Electron main already owns its native PiP stack");
    napi_value names=Get(env,args[0],"controlText");Keys(env,names,{"closeTooltip","hide","hideForGroup","hideForAllGroups","placementTooltip"});auto text=[OCTPIPStackControlText new];
    text.closeTooltip=NSStringValue(env,Get(env,names,"closeTooltip"));text.hide=NSStringValue(env,Get(env,names,"hide"));text.hideForGroup=NSStringValue(env,Get(env,names,"hideForGroup"));text.hideForAllGroups=NSStringValue(env,Get(env,names,"hideForAllGroups"));text.placementTooltip=NSStringValue(env,Get(env,names,"placementTooltip"));
    NSString* asset=NSStringValue(env,Get(env,args[0],"placementAssetPath"));Require(asset.isAbsolutePath&&[asset isEqual:asset.stringByResolvingSymlinksInPath],"PiP placement asset must be a canonical absolute path");
    struct stat statbuf{};Require(lstat(asset.fileSystemRepresentation,&statbuf)==0&&S_ISREG(statbuf.st_mode)&&statbuf.st_size==3767,"PiP placement asset differs from the owned source");
    NSData* asset_bytes=[NSData dataWithContentsOfFile:asset];unsigned char digest[CC_SHA256_DIGEST_LENGTH];CC_SHA256(asset_bytes.bytes,static_cast<CC_LONG>(asset_bytes.length),digest);
    constexpr unsigned char expected[]={0x89,0x79,0xfa,0x7c,0xe7,0xb7,0xda,0xbf,0x97,0xab,0x3e,0x73,0xc5,0xe7,0x65,0x2c,0xbf,0x55,0x64,0xa9,0x4e,0xd7,0xcd,0x21,0x3f,0xa0,0x70,0xee,0xbc,0x25,0x19,0xea};
    Require(asset_bytes.length==3767&&std::memcmp(digest,expected,32)==0,"PiP placement asset hash differs");
    NSString* settingsPath=NSStringValue(env,Get(env,args[0],"settingsAssetBundlePath"));
    Require(settingsPath.isAbsolutePath&&[settingsPath isEqual:settingsPath.stringByResolvingSymlinksInPath],"Settings image resources must be canonical");
    NSDictionary* resourceHashes=@{@"Contents/Info.plist":@"9fc985fd70f134c1eb5971e4b93ad52aef8ba6df2157ab8ae532eb4684a1bbfe",@"Contents/Resources/Assets.car":@"f20daabb37db9e1a92f1af8d108c55683603b2c13633b3bc9d1d88524b41ccbb"};
    for(NSString* relative in resourceHashes){
      NSString* file=[settingsPath stringByAppendingPathComponent:relative];struct stat info{};
      Require([file isEqual:file.stringByResolvingSymlinksInPath]&&lstat(file.fileSystemRepresentation,&info)==0&&S_ISREG(info.st_mode)&&info.st_size>0&&info.st_size<=28632,"Settings resource is not a bounded regular file");
      NSData* bytes=[NSData dataWithContentsOfFile:file];unsigned char digest[CC_SHA256_DIGEST_LENGTH];CC_SHA256(bytes.bytes,static_cast<CC_LONG>(bytes.length),digest);
      NSMutableString* hash=[NSMutableString string];for(unsigned char value:digest)[hash appendFormat:@"%02x",value];
      Require([hash isEqual:resourceHashes[relative]],"Settings image resource differs from its source");
    }
    NSBundle* settingsResources=[NSBundle bundleWithPath:settingsPath];Require(settingsResources!=nil&&!settingsResources.executableURL,"Settings resources must contain images only");
    auto binding=std::make_shared<Binding>();binding->settingsResources=settingsResources;napi_value callback=Get(env,args[0],"onEvent");napi_valuetype type;Check(napi_typeof(env,callback,&type));Require(type==napi_function,"PiP event handler must be a function");napi_value resource,name;Check(napi_create_object(env,&resource));name=Text(env,"OctopusNativePIP.events");
    Check(napi_create_threadsafe_function(env,callback,resource,name,1024,1,nullptr,nullptr,nullptr,CallEvent,&binding->events));Check(napi_unref_threadsafe_function(env,binding->events));
    binding->host=octopus::pip::ReceiverBootstrap::Create(text,[NSURL fileURLWithPath:asset],[weak=std::weak_ptr<Binding>(binding)](NSDictionary* event){Emit(weak,event);});
    napi_value object;Check(napi_create_object(env,&object));
    const napi_property_descriptor methods[]={
      {"prepareAutomationPermission",nullptr,Call<Method::PrepareAutomation>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"beginAutomationPermission",nullptr,Call<Method::BeginAutomation>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"cancelAutomationSettings",nullptr,Call<Method::CancelAutomationSettings>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"showPermissions",nullptr,Call<Method::ShowPermissions>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"updatePermissions",nullptr,Call<Method::UpdatePermissions>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"updatePermissionSettings",nullptr,Call<Method::UpdateSettings>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"closePermissions",nullptr,Call<Method::ClosePermissions>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"registerHost",nullptr,Call<Method::RegisterHost>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"removeHost",nullptr,Call<Method::RemoveHost>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"beginSource",nullptr,Call<Method::BeginSource>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"installSource",nullptr,Call<Method::InstallSource>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"cancelSource",nullptr,Call<Method::CancelSource>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"revokeSource",nullptr,Call<Method::RevokeSource>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"upsertBrowserImage",nullptr,Call<Method::UpsertBrowserImage>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"invalidateBrowserImage",nullptr,Call<Method::InvalidateBrowserImage>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"completeGroup",nullptr,Call<Method::CompleteGroup>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"clearCompletedGroup",nullptr,Call<Method::ClearCompletedGroup>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"setSelection",nullptr,Call<Method::Selection>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"setPlacement",nullptr,Call<Method::Placement>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"moveToHost",nullptr,Call<Method::MoveToHost>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"setMaxDisplaySize",nullptr,Call<Method::MaxDisplaySize>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"status",nullptr,Call<Method::Status>,nullptr,nullptr,nullptr,napi_default,nullptr},
      {"stop",nullptr,Call<Method::Stop>,nullptr,nullptr,nullptr,napi_default,nullptr}
    };
    Check(napi_define_properties(env,object,sizeof(methods)/sizeof(methods[0]),methods));Check(napi_type_tag_object(env,object,&kReceiverTag));auto retained=std::make_unique<std::shared_ptr<Binding>>(binding);
    Check(napi_wrap(env,object,retained.get(),[](napi_env,void* raw,void*){delete static_cast<std::shared_ptr<Binding>*>(raw);},nullptr,nullptr));retained.release();environment->binding=binding;return object;
  }catch(const std::exception& error){napi_throw_error(env,nullptr,error.what());return nullptr;}
}
napi_value Init(napi_env env,napi_value exports) {
  auto environment=std::make_unique<Environment>();napi_value create;
  if(napi_create_function(env,"createPiPHost",NAPI_AUTO_LENGTH,Create,environment.get(),&create)!=napi_ok||napi_set_named_property(env,exports,"createPiPHost",create)!=napi_ok||napi_add_env_cleanup_hook(env,[](void* raw){auto* environment=static_cast<Environment*>(raw);if(auto binding=environment->binding.lock())binding->Close();delete environment;},environment.get())!=napi_ok){napi_throw_error(env,nullptr,"PiP Node-API initialization failed");return nullptr;}
  environment.release();return exports;
}
}
NAPI_MODULE(octopus_pip_receiver,Init)
