#include "bootstrap.h"
#import "permissions.h"
#import <CommonCrypto/CommonHMAC.h>
#import <CommonCrypto/CommonDigest.h>
#import <Security/SecRandom.h>
#import <objc/message.h>
#include <mach/mach_time.h>
#include <dlfcn.h>
#include <unistd.h>
#include <array>
#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstring>
#include <deque>
#include <limits>
#include <map>
#include <set>
#include <stdexcept>
#include <tuple>
#include <vector>

@interface NSXPCListenerEndpoint (OCTPIPNativeEndpoint)
- (void)_setEndpoint:(xpc_object_t)endpoint;
@end
@interface NSXPCInterface (OCTPIPNativeFence)
- (void)setXPCType:(xpc_type_t)type forSelector:(SEL)selector
    argumentIndex:(NSUInteger)index ofReply:(BOOL)reply;
@end

namespace octopus::pip {
namespace {
constexpr size_t kMaxBindings = 1024;
constexpr size_t kMaxBindingBytes = 65536;
void Require(bool value, const char* text) { if (!value) throw std::runtime_error(text); }
void Main() { Require([NSThread isMainThread], "Native PiP bootstrap requires Electron main"); }
NSString* Text(const std::string& value) { return [[NSString alloc] initWithBytes:value.data() length:value.size() encoding:NSUTF8StringEncoding]; }
std::string Name(id value) {
  Require([value isKindOfClass:NSString.class] && [value length] > 0 && [value lengthOfBytesUsingEncoding:NSUTF8StringEncoding] <= kMaxBindingBytes,
          "Native PiP identity is invalid");
  NSData* bytes = [value dataUsingEncoding:NSUTF8StringEncoding];
  std::string result(static_cast<const char*>(bytes.bytes), bytes.length);
  Require(result.find('\0') == std::string::npos, "Native PiP identity contains a null byte");
  return result;
}
void Keys(id value, std::initializer_list<NSString*> keys) {
  Require([value isKindOfClass:NSDictionary.class] && [value count] == keys.size(), "Native PiP object fields differ");
  for (NSString* key : keys) Require(value[key] != nil, "Native PiP object field is absent");
}
uint64_t Number(id value, uint64_t maximum = kMaxSequence) {
  Require([value isKindOfClass:NSNumber.class] && CFGetTypeID((__bridge CFTypeRef)value) != CFBooleanGetTypeID(), "Native PiP counter must be numeric");
  double number = [value doubleValue];
  Require(std::isfinite(number) && number > 0 && number <= maximum && std::floor(number) == number, "Native PiP counter is invalid");
  return [value unsignedLongLongValue];
}
uint64_t Decimal(id value) {
  auto text = Name(value); uint64_t result = 0;
  Require(text.front() >= '1' && text.front() <= '9', "Native PiP decimal is not canonical");
  for (char ch : text) {
    Require(ch >= '0' && ch <= '9' && result <= (UINT64_MAX - (ch - '0')) / 10, "Native PiP decimal exceeds UInt64");
    result = result * 10 + (ch - '0');
  }
  return result;
}
std::string Hex(id value, size_t length) {
  auto text = Name(value); Require(text.size() == length, "Native PiP nonce length differs");
  for (char ch : text) Require((ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f'), "Native PiP nonce is not canonical hex");
  return text;
}
std::array<uint8_t, 32> Random() {
  std::array<uint8_t,32> bytes{};
  Require(SecRandomCopyBytes(kSecRandomDefault, bytes.size(), bytes.data()) == errSecSuccess, "Native PiP entropy is unavailable");
  return bytes;
}
NSString* RandomHex() {
  auto bytes = Random(); std::string out; out.reserve(64); constexpr char hex[] = "0123456789abcdef";
  for (auto byte : bytes) { out += hex[byte >> 4]; out += hex[byte & 15]; }
  return Text(out);
}
void Erase(void* pointer, size_t length) { volatile uint8_t* bytes = static_cast<volatile uint8_t*>(pointer); while (length--) *bytes++ = 0; }
struct Secret {
  std::array<uint8_t, 32> bytes{};
  ~Secret() { Erase(bytes.data(), bytes.size()); }
  void Clear() { Erase(bytes.data(), bytes.size()); }
};
uint64_t NanosecondsLeft(uint64_t deadline) {
  uint64_t now = mach_continuous_time(); Require(now < deadline, "Native PiP bootstrap deadline expired");
  mach_timebase_info_data_t scale{};
  Require(mach_timebase_info(&scale) == KERN_SUCCESS && scale.denom && scale.numer, "Native PiP continuous clock is unavailable");
  __uint128_t nanos = static_cast<__uint128_t>(deadline - now) * scale.numer / scale.denom;
  Require(nanos > 0 && nanos <= INT64_MAX, "Native PiP deadline exceeds its clock bound");
  return static_cast<uint64_t>(nanos);
}
uint64_t Deadline(uint64_t milliseconds) {
  mach_timebase_info_data_t scale{}; Require(mach_timebase_info(&scale) == KERN_SUCCESS && scale.numer && scale.denom, "Native PiP clock is unavailable");
  __uint128_t ticks = static_cast<__uint128_t>(milliseconds) * 1000000 * scale.denom / scale.numer;
  uint64_t now = mach_continuous_time(); Require(ticks && ticks < UINT64_MAX - now, "Native PiP deadline overflows");
  return now + static_cast<uint64_t>(ticks);
}
NSError* Failure(NSString* message) { return [NSError errorWithDomain:@"OctopusNativePIP" code:1 userInfo:@{NSLocalizedDescriptionKey:message}]; }
NSDictionary* IdentityObject(const Identity& identity) { return @{ @"hostGeneration":Text(identity.host_generation), @"presentationId":Text(identity.presentation_id), @"streamGeneration":@(identity.stream_generation) }; }
Identity ReadIdentity(NSDictionary* value) {
  Keys(value, {@"hostGeneration", @"presentationId", @"streamGeneration"});
  return {Name(value[@"hostGeneration"]), Name(value[@"presentationId"]), Number(value[@"streamGeneration"])};
}
using IdentityKey = std::tuple<std::string, std::string, uint64_t>;
IdentityKey Key(const Identity& value) { return {value.host_generation,value.presentation_id,value.stream_generation}; }
Identity ReadIdentity(OCTPIPStackIdentity* value) { return {Name(value.hostGeneration),Name(value.presentationID),value.streamGeneration}; }

class UniqueJSON {
 public:
  explicit UniqueJSON(NSData* data) : bytes_(static_cast<const char*>(data.bytes), data.length) {}
  void Check() { Value(0); Space(); Require(offset_ == bytes_.size(), "Native PiP binding has trailing bytes"); }
 private:
  void Space() { while (offset_ < bytes_.size() && (bytes_[offset_] == ' ' || bytes_[offset_] == '\n' || bytes_[offset_] == '\r' || bytes_[offset_] == '\t')) ++offset_; }
  char Take() { Require(offset_ < bytes_.size(), "Native PiP binding is truncated"); return bytes_[offset_++]; }
  std::string String() {
    size_t begin = offset_; Require(Take() == '"', "Native PiP binding key is invalid"); bool escaped = false;
    for (;;) {
      char ch = Take();
      if (!escaped && ch == '"') break;
      if (!escaped && ch == '\\') escaped = true; else escaped = false;
    }
    NSData* raw = [NSData dataWithBytes:bytes_.data()+begin length:offset_-begin]; NSError* error = nil;
    id value = [NSJSONSerialization JSONObjectWithData:raw options:NSJSONReadingFragmentsAllowed error:&error];
    Require(!error && [value isKindOfClass:NSString.class], "Native PiP binding string is invalid");
    NSData* utf8 = [value dataUsingEncoding:NSUTF8StringEncoding];
    return std::string(static_cast<const char*>(utf8.bytes),utf8.length);
  }
  void Value(size_t depth) {
    Require(depth <= 16, "Native PiP binding nesting exceeds its bound"); Space(); Require(offset_ < bytes_.size(), "Native PiP binding is truncated");
    char head = bytes_[offset_];
    if (head == '"') { String(); return; }
    if (head == '{' || head == '[') {
      ++offset_; Space(); char end = head == '{' ? '}' : ']'; std::set<std::string> keys; size_t count = 0;
      if (offset_ < bytes_.size() && bytes_[offset_] == end) { ++offset_; return; }
      for (;;) {
        Require(++count <= kMaxBindings, "Native PiP binding collection exceeds its bound"); Space();
        if (head == '{') { Require(keys.insert(String()).second, "Native PiP binding contains a duplicate field"); Space(); Require(Take() == ':', "Native PiP binding colon is absent"); }
        Value(depth + 1); Space(); char separator = Take(); if (separator == end) return;
        Require(separator == ',', "Native PiP binding separator is invalid");
      }
    }
    size_t begin = offset_;
    while (offset_ < bytes_.size() && bytes_[offset_] != ',' && bytes_[offset_] != '}' && bytes_[offset_] != ']' && bytes_[offset_] != ' ' && bytes_[offset_] != '\n' && bytes_[offset_] != '\r' && bytes_[offset_] != '\t') ++offset_;
    Require(offset_ > begin, "Native PiP binding scalar is invalid");
  }
  std::string bytes_; size_t offset_ = 0;
};
NSDictionary* Parse(NSData* raw) {
  Require([raw isKindOfClass:NSData.class] && raw.length && raw.length <= kMaxBindingBytes, "Native PiP binding bytes exceed their bound");
  UniqueJSON(raw).Check(); NSError* error = nil;
  id value = [NSJSONSerialization JSONObjectWithData:raw options:0 error:&error];
  Require(!error && [value isKindOfClass:NSDictionary.class], "Native PiP binding JSON is invalid"); return value;
}
void Scope(NSDictionary* scope) {
  Keys(scope,{@"host_generation",@"worker_environment_id",@"formation_pin",@"app_instance_id",@"grant_request_id"});
  for (NSString* key in scope) Name(scope[key]); Hex(scope[@"host_generation"],32);
}
NSData* Proof(const Secret& secret, NSData* binding, NSData* receiver, NSData* producer, uint8_t role) {
  Require(receiver.length == 32 && producer.length == 32 && binding.length <= kMaxBindingBytes, "Native PiP proof input is invalid");
  constexpr char domain[] = "octopus.native-pip.v1.auth\n";
  CCHmacContext context; CCHmacInit(&context,kCCHmacAlgSHA256,secret.bytes.data(),secret.bytes.size());
  CCHmacUpdate(&context,domain,sizeof(domain)-1); uint32_t size = static_cast<uint32_t>(binding.length);
  uint8_t length[] = {uint8_t(size>>24),uint8_t(size>>16),uint8_t(size>>8),uint8_t(size)};
  CCHmacUpdate(&context,length,sizeof(length)); CCHmacUpdate(&context,binding.bytes,binding.length);
  CCHmacUpdate(&context,receiver.bytes,32); CCHmacUpdate(&context,producer.bytes,32); CCHmacUpdate(&context,&role,1);
  uint8_t output[CC_SHA256_DIGEST_LENGTH]; CCHmacFinal(&context,output); NSData* result = [NSData dataWithBytes:output length:sizeof(output)];
  Erase(output,sizeof(output)); Erase(&context,sizeof(context)); return result;
}
bool EqualProof(NSData* left, NSData* right) {
  if (![left isKindOfClass:NSData.class] || ![right isKindOfClass:NSData.class] || left.length != 32 || right.length != 32) return false;
  const uint8_t* a = static_cast<const uint8_t*>(left.bytes); const uint8_t* b = static_cast<const uint8_t*>(right.bytes); volatile uint8_t difference = 0;
  for (size_t i = 0; i < 32; ++i) difference = difference | (a[i] ^ b[i]); return difference == 0;
}
struct Rendezvous {
  NSMachPort* __strong port = nil;
  std::atomic<bool> cancelled{false};
  bool owns_send_right = false;
  void Cancel() {
    if (cancelled.exchange(true)) return;
    if (owns_send_right && MACH_PORT_VALID(port.machPort)) mach_port_deallocate(mach_task_self(),port.machPort);
    owns_send_right = false; [port invalidate];
  }
  ~Rendezvous() { Cancel(); }
};
NSXPCInterface* PublisherInterface() {
  auto interface = [NSXPCInterface interfaceWithProtocol:@protocol(OCTPIPPublisher)];
  NSSet* identity = [NSSet setWithObjects:NSDictionary.class,NSString.class,NSNumber.class,nil];
  for (NSString* name in @[@"didEndPresentation:reply:",@"requestFocus:reply:"])
    [interface setClasses:identity forSelector:NSSelectorFromString(name) argumentIndex:0 ofReply:NO];
  [interface setClasses:identity forSelector:@selector(setMaxDisplaySize:identity:reply:) argumentIndex:1 ofReply:NO];
  return interface;
}
NSXPCInterface* ReceiverInterface() {
  auto interface = [NSXPCInterface interfaceWithProtocol:@protocol(OCTPIPReceiver)];
  NSSet* identity = [NSSet setWithObjects:NSDictionary.class,NSString.class,NSNumber.class,nil];
  for (NSString* name in @[@"publishPresentation:contextID:width:height:reply:", @"preparePresentation:operationID:kind:contextID:width:height:fence:reply:", @"completePresentation:operationID:reply:", @"invalidatePresentation:reason:reply:", @"willEndPresentation:reply:", @"noteInteraction:reply:"]) {
    [interface setClasses:identity forSelector:NSSelectorFromString(name) argumentIndex:0 ofReply:NO];
  }
  Require([interface respondsToSelector:@selector(setXPCType:forSelector:argumentIndex:ofReply:)], "Native PiP XPC fence transport is unavailable");
  [interface setXPCType:XPC_TYPE_DICTIONARY forSelector:@selector(preparePresentation:operationID:kind:contextID:width:height:fence:reply:) argumentIndex:6 ofReply:NO];
  return interface;
}
}
}

@interface OCTPIPReceiverExport : NSObject <OCTPIPReceiver> {
 @public std::weak_ptr<octopus::pip::ReceiverBootstrap> owner;
 @public std::shared_ptr<std::atomic<size_t>> in_flight;
}
@end

namespace octopus::pip {
struct ReceiverBootstrap::Impl {
  struct Pending;
  struct Channel {
    std::string host; pid_t producer = 0; NSXPCConnection* __strong connection = nil;
    OCTPIPReceiverExport* __strong exported = nil; std::shared_ptr<Rendezvous> rendezvous;
    bool established = false; bool connected = false; bool closed = false; bool authenticating = false;
    std::deque<std::shared_ptr<Pending>> queue;
  };
  struct Pending {
    enum class Phase { AwaitingPrivateResponse, Queued, Authenticating, Activating, Active, Ending, Cancelled };
    Phase phase = Phase::AwaitingPrivateResponse;
    NSDictionary* __strong expected = nil; NSDictionary* __strong binding = nil;
    NSData* __strong bytes = nil; NSData* __strong challenge = nil;
    NSString* __strong nonce = nil; NSString* __strong bootstrap = nil; Secret key;
    std::string request; uint64_t deadline = 0; Identity identity;
    std::weak_ptr<Channel> channel;
    bool completed = false;
    NSMutableArray* __strong invalidation_replies = [NSMutableArray array];
  };
  struct Entry { std::shared_ptr<Receiver> receiver; std::shared_ptr<Pending> source; std::shared_ptr<Channel> channel; };
  struct BrowserImage {
    Identity identity; std::string group, batch; uint64_t sequence;
    std::array<uint8_t, CC_SHA256_DIGEST_LENGTH> digest;
  };
  struct PermissionSource { OCTAutomationPermission* __strong permission; NSDictionary* __strong scope; NSString* __strong window; };
  std::map<std::string,PermissionSource> permissions;
  void RemovePermission(const std::string& request) {
    auto found=permissions.find(request);if(found==permissions.end())return;
    auto permission=found->second.permission;permissions.erase(found);
    for(const auto& [id,source]:permissions)if(source.permission==permission)return;
    [permission invalidate];
  }
  ReceiverBootstrap* owner = nullptr; OnEvent on_event; bool stopped = false;
  NSString* __strong instance_nonce = nil; OCTPIPStack* __strong stack = nil;
  std::shared_ptr<StackHostOwner> hosts;
  std::map<std::string,std::shared_ptr<Pending>> pending;
  std::map<std::pair<std::string,pid_t>,std::shared_ptr<Channel>> channels;
  std::map<IdentityKey,Entry> receivers;
  std::map<IdentityKey,BrowserImage> browser_images;
  std::shared_ptr<std::atomic<size_t>> in_flight = std::make_shared<std::atomic<size_t>>(0);
  void Emit(NSDictionary* value) { if (on_event && !stopped) on_event(value); }
  bool Current(const std::shared_ptr<Pending>& item) const {
    auto found = pending.find(item->request); return !stopped && found != pending.end() && found->second == item && item->phase != Pending::Phase::Cancelled;
  }
  void RequirePending(const std::shared_ptr<Pending>& item, const std::shared_ptr<Channel>& channel) {
    Require(Current(item) && item->phase != Pending::Phase::Active && !channel->closed && item->channel.lock() == channel && channel->connection && channel->connection.processIdentifier == channel->producer,
            "Native PiP pending source or peer is no longer current"); NanosecondsLeft(item->deadline);
  }
  void ResolveInvalidations(const std::shared_ptr<Pending>& item, NSError* error) {
    NSArray* replies = [item->invalidation_replies copy]; [item->invalidation_replies removeAllObjects];
    for (id value in replies) { void (^reply)(NSError*) = value; reply(error); }
  }
  void SourceEvent(const std::shared_ptr<Pending>& item, NSString* type) {
    Emit(@{@"type":type,@"requestId":Text(item->request),@"receiverNonce":item->nonce});
  }
  void CloseChannel(std::shared_ptr<Channel> channel, NSString* pending_type = @"source-failed") {
    if (channel->closed) return; channel->closed = true;
    if (channel->rendezvous) channel->rendezvous->Cancel(); channel->rendezvous.reset();
    auto connection = channel->connection; channel->connection = nil;
    connection.interruptionHandler = nil; connection.invalidationHandler = nil; [connection invalidate];
    for (auto it = receivers.begin(); it != receivers.end();) {
      if (it->second.channel != channel) { ++it; continue; }
      auto source = it->second.source; auto receiver = it->second.receiver; it = receivers.erase(it);
      source->phase = Pending::Phase::Cancelled; source->key.Clear(); receiver->ConnectionLost(connection); receiver->Stop(); ResolveInvalidations(source,Failure(@"Native PiP channel closed"));
      SourceEvent(source,@"source-disconnected"); pending.erase(source->request);
    }
    auto queue = std::move(channel->queue);
    for (auto& item : queue) if (Current(item)) { item->phase = Pending::Phase::Cancelled; item->key.Clear(); SourceEvent(item,pending_type); pending.erase(item->request); }
    channels.erase({channel->host,channel->producer});
  }
  void Fail(std::shared_ptr<Pending> item, NSString* type = @"source-failed") {
    if (!Current(item)) return; auto channel = item->channel.lock();
    if (channel && !channel->established) { CloseChannel(channel); return; }
    item->phase = Pending::Phase::Cancelled; item->key.Clear(); ResolveInvalidations(item,Failure(@"Native PiP source was withdrawn"));
    auto found = receivers.find(Key(item->identity));
    if (found != receivers.end() && found->second.source == item) { auto receiver = found->second.receiver; receivers.erase(found); receiver->Stop(); }
    SourceEvent(item,type); pending.erase(item->request);
    if (channel && !channel->closed) {
      if (item->bootstrap) {
        id<OCTPIPPublisher> peer = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {}];
        [peer cancelSource:item->bootstrap reply:^(NSError*) {}];
      }
      bool was_front = !channel->queue.empty() && channel->queue.front() == item;
      std::erase(channel->queue,item); if (was_front) channel->authenticating = false;
      Authenticate(channel);
    }
  }
  void End(std::shared_ptr<Pending> item) {
    auto found = receivers.find(Key(item->identity));
    if (found == receivers.end() || found->second.source != item || item->phase == Pending::Phase::Ending) return;
    auto receiver = found->second.receiver; auto channel = found->second.channel;
    item->phase = Pending::Phase::Ending; auto weak = owner->weak_from_this();
    receiver->End(channel->connection,item->identity,[weak,item,channel] {
      if (auto self = weak.lock()) {
        auto& state = *self->impl_; auto found = state.receivers.find(Key(item->identity));
        if (found == state.receivers.end() || found->second.source != item) return;
        state.receivers.erase(found); state.pending.erase(item->request); item->phase = Pending::Phase::Cancelled;
        state.ResolveInvalidations(item,nil); state.SourceEvent(item,@"source-ended"); state.RemovePermission(item->request);
        if (!channel->closed && channel->connection) {
          id<OCTPIPPublisher> peer = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {}];
          [peer didEndPresentation:IdentityObject(item->identity) reply:^(NSError*) {}];
        }
      }
    });
  }
  void Authenticate(std::shared_ptr<Channel> channel) {
    if (stopped || channel->closed || !channel->connected || channel->authenticating || channel->queue.empty()) return;
    auto item = channel->queue.front(); if (!Current(item)) { channel->queue.pop_front(); Authenticate(channel); return; }
    try {
      RequirePending(item,channel); channel->authenticating = true; item->phase = Pending::Phase::Authenticating;
      auto random = Random(); item->challenge = [NSData dataWithBytes:random.data() length:random.size()];
      auto weak = owner->weak_from_this();
      id<OCTPIPPublisher> peer = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {
        dispatch_async(dispatch_get_main_queue(), ^{ if (auto self = weak.lock()) self->impl_->Fail(item); });
      }];
      [peer beginSourceAuthentication:item->bytes receiverChallenge:item->challenge reply:^(NSData* producer, NSData* proof, NSError* error) {
        dispatch_async(dispatch_get_main_queue(), ^{
          if (auto self = weak.lock()) {
            auto& state = *self->impl_;
            try {
              state.RequirePending(item,channel); Require(!error && [producer isKindOfClass:NSData.class] && producer.length == 32 && EqualProof(Proof(item->key,item->bytes,item->challenge,producer,2),proof), "Native PiP producer authentication failed");
              NSData* receiver_proof = Proof(item->key,item->bytes,item->challenge,producer,1);
              id<OCTPIPPublisher> current = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {
                dispatch_async(dispatch_get_main_queue(), ^{ if (auto live = weak.lock()) live->impl_->Fail(item); });
              }];
              [current finishSourceAuthentication:item->bootstrap receiverProof:receiver_proof reply:^(NSError* failure) {
                dispatch_async(dispatch_get_main_queue(), ^{
                  if (auto live = weak.lock()) {
                    auto& state = *live->impl_;
                    try {
                      state.RequirePending(item,channel); Require(!failure && !state.receivers.contains(Key(item->identity)), "Native PiP source authentication was not accepted");
                      item->key.Clear(); item->challenge = nil; item->phase = Pending::Phase::Activating;
                      auto receiver = std::make_shared<Receiver>(item->identity,[weak](const Event& event) {
                        if (auto client = weak.lock()) client->impl_->Emit(@{@"type":Text(event.type),@"identity":IdentityObject(event.identity),@"operationId":@(event.operation),@"width":@(event.size.width),@"height":@(event.size.height)});
                      });
                      receiver->InstallAuthenticatedConnection(channel->connection,item->identity,state.stack,state.hosts,item->expected[@"groupId"],item->expected[@"batchId"]);
                      state.receivers.emplace(Key(item->identity),Entry{receiver,item,channel});
                      id<OCTPIPPublisher> active = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {
                        dispatch_async(dispatch_get_main_queue(), ^{ if (auto live = weak.lock()) live->impl_->Fail(item); });
                      }];
                      [active activateSource:item->bootstrap reply:^(NSError* activation_error) {
                        dispatch_async(dispatch_get_main_queue(), ^{
                          if (auto current = weak.lock()) {
                            auto& state = *current->impl_;
                            try {
                              state.RequirePending(item,channel); Require(!activation_error, "Native PiP source activation failed");
                              item->phase = Pending::Phase::Active; item->bytes = nil; channel->established = true;
                              Require(!channel->queue.empty() && channel->queue.front() == item, "Native PiP authentication queue changed");
                              channel->queue.pop_front(); channel->authenticating = false; state.SourceEvent(item,@"source-authenticated"); state.Authenticate(channel);
                            } catch (...) { state.Fail(item); }
                          }
                        });
                      }];
                    } catch (...) { state.Fail(item); }
                  }
                });
              }];
            } catch (...) { state.Fail(item); }
          }
        });
      }];
    } catch (...) { Fail(item); }
  }
  void Connect(const std::shared_ptr<Channel>& channel, NSXPCListenerEndpoint* endpoint) {
    if (stopped || channel->closed || channel->connection || channel->queue.empty()) return;
    try {
      auto item = channel->queue.front(); Require(Current(item), "Native PiP bootstrap source was cancelled"); NanosecondsLeft(item->deadline);
      NSXPCConnection* connection = [[NSXPCConnection alloc] initWithListenerEndpoint:endpoint]; Require(connection != nil, "Native PiP endpoint connection failed");
      auto weak = owner->weak_from_this(); auto exported = [OCTPIPReceiverExport new]; exported->owner = weak; exported->in_flight = in_flight;
      connection.remoteObjectInterface = PublisherInterface(); connection.exportedInterface = ReceiverInterface(); connection.exportedObject = exported;
      channel->connection = connection; channel->exported = exported;
      auto weak_channel = std::weak_ptr<Channel>(channel);
      void (^lost)() = ^{ dispatch_async(dispatch_get_main_queue(), ^{
        if (auto self = weak.lock()) if (auto current = weak_channel.lock()) self->impl_->CloseChannel(current);
      }); };
      connection.interruptionHandler = lost; connection.invalidationHandler = lost; [connection activate];
      id<OCTPIPPublisher> peer = [connection remoteObjectProxyWithErrorHandler:^(NSError*) { lost(); }];
      [peer connectWithReply:^(NSError* error) {
        dispatch_async(dispatch_get_main_queue(), ^{
          if (auto self = weak.lock()) {
            try {
              Require(!error && !channel->closed && channel->connection == connection && connection.processIdentifier == channel->producer && channel->producer != getpid(), "Native PiP endpoint process does not match the installed producer");
              channel->connected = true; self->impl_->Authenticate(channel);
            } catch (...) { self->impl_->CloseChannel(channel); }
          }
        });
      }];
    } catch (...) { CloseChannel(channel); }
  }
  void RendezvousStart(const std::shared_ptr<Channel>& channel, const std::shared_ptr<Pending>& item) {
    using Receive = int (*)(mach_port_t, xpc_object_t __strong*);
    using Reply = int (*)(xpc_object_t);
    auto receive = reinterpret_cast<Receive>(dlsym(RTLD_DEFAULT,"xpc_pipe_receive"));
    auto reply = reinterpret_cast<Reply>(dlsym(RTLD_DEFAULT,"xpc_pipe_routine_reply"));
    Require(receive && reply && [NSXPCListenerEndpoint instancesRespondToSelector:@selector(_setEndpoint:)], "Native PiP endpoint transport capability is unavailable");
    auto rendezvous = std::make_shared<Rendezvous>(); id native_port = [NSMachPort port];
    Require([native_port isKindOfClass:NSMachPort.class], "Native PiP rendezvous did not create a Mach port");
    rendezvous->port = native_port;
    mach_port_t port = rendezvous->port.machPort; Require(MACH_PORT_VALID(port), "Native PiP rendezvous receive right is unavailable");
    Require(mach_port_insert_right(mach_task_self(),port,port,MACH_MSG_TYPE_MAKE_SEND) == KERN_SUCCESS, "Native PiP rendezvous send right is unavailable");
    rendezvous->owns_send_right = true; channel->rendezvous = rendezvous; auto weak = owner->weak_from_this();
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0), ^{
      @autoreleasepool {
        xpc_object_t message = nil; int error = receive(port,&message); NSXPCListenerEndpoint* endpoint = nil;
        if (!error && !rendezvous->cancelled.load() && message && xpc_get_type(message) == XPC_TYPE_DICTIONARY) {
          xpc_object_t value = xpc_dictionary_get_value(message,"endpoint");
          if (value && xpc_get_type(value) == XPC_TYPE_ENDPOINT) {
            endpoint = [NSXPCListenerEndpoint new]; [endpoint _setEndpoint:value];
            xpc_object_t acknowledgement = xpc_dictionary_create_reply(message);
            if (!acknowledgement || reply(acknowledgement) != 0) endpoint = nil;
          }
        }
        dispatch_async(dispatch_get_main_queue(), ^{
          if (auto self = weak.lock()) {
            if (channel->closed || channel->rendezvous != rendezvous || rendezvous->cancelled.load()) return;
            rendezvous->Cancel(); channel->rendezvous.reset();
            if (endpoint) self->impl_->Connect(channel,endpoint); else self->impl_->CloseChannel(channel);
          }
        });
      }
    });
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0), ^{
      @autoreleasepool {
        bool failed = true; bool permission_required = false;
        try {
          double seconds = static_cast<double>(NanosecondsLeft(item->deadline))/1e9;
          if (!rendezvous->cancelled.load()) {
            NSAppleEventDescriptor* event = [NSAppleEventDescriptor appleEventWithEventClass:'OcPi' eventID:'BtV1' targetDescriptor:[NSAppleEventDescriptor descriptorWithProcessIdentifier:channel->producer] returnID:kAutoGenerateReturnID transactionID:kAnyTransactionID];
            [event setParamDescriptor:[NSAppleEventDescriptor descriptorWithString:item->bootstrap] forKeyword:'BtID'];
            [event setAttributeDescriptor:[NSAppleEventDescriptor descriptorWithDescriptorType:'port' bytes:&port length:sizeof(port)] forKeyword:'repp'];
            NSError* error = nil;
            const auto options = static_cast<NSAppleEventSendOptions>(NSAppleEventSendWaitForReply | NSAppleEventSendNeverInteract | kAEDoNotPromptForUserConsent);
            [event sendEventWithOptions:options timeout:seconds error:&error]; failed = error != nil;
            permission_required = [error.domain isEqual:NSOSStatusErrorDomain] && (error.code == errAEEventWouldRequireUserConsent || error.code == errAEEventNotPermitted);
          }
        } catch (...) {}
        if (failed) dispatch_async(dispatch_get_main_queue(), ^{
          if (auto self = weak.lock()) if (!channel->connected && !channel->closed) self->impl_->CloseChannel(channel,permission_required ? @"source-permission-required" : @"source-failed");
        });
      }
    });
  }
  bool HasIdentity(OCTPIPStackIdentity* value) const {
    try {
      if (stopped) return false;
      const auto key = Key(ReadIdentity(value));
      if (browser_images.contains(key)) return true;
      auto found = receivers.find(key); return found != receivers.end() && found->second.receiver->MatchesIdentity(value);
    } catch (...) { return false; }
  }
  void SendFocus(OCTPIPStackIdentity* value) {
    const auto key = Key(ReadIdentity(value));
    auto browser = browser_images.find(key);
    if (browser != browser_images.end()) {
      Emit(@{@"type":@"focus-browser",@"identity":IdentityObject(browser->second.identity),@"groupId":Text(browser->second.group),@"batchId":Text(browser->second.batch)});
      return;
    }
    auto found = receivers.find(key); if (found == receivers.end()) return;
    auto channel = found->second.channel; auto identity = found->second.source->identity; auto weak = owner->weak_from_this();
    id<OCTPIPPublisher> peer = [channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {
      dispatch_async(dispatch_get_main_queue(), ^{ if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"focus-unavailable",@"identity":IdentityObject(identity)}); });
    }];
    [peer requestFocus:IdentityObject(identity) reply:^(NSError* error) {
      if (error) dispatch_async(dispatch_get_main_queue(), ^{ if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"focus-unavailable",@"identity":IdentityObject(identity)}); });
    }];
  }
};

ReceiverBootstrap::ReceiverBootstrap() : impl_(std::make_unique<Impl>()) { impl_->owner = this; }
std::shared_ptr<ReceiverBootstrap> ReceiverBootstrap::Create(OCTPIPStackControlText* text, NSURL* asset, OnEvent on_event) {
  Main(); auto result = std::shared_ptr<ReceiverBootstrap>(new ReceiverBootstrap); result->Initialize(text,asset,std::move(on_event)); return result;
}
void ReceiverBootstrap::Initialize(OCTPIPStackControlText* text, NSURL* asset, OnEvent on_event) {
  Main(); impl_->on_event = std::move(on_event); impl_->instance_nonce = RandomHex();
  auto weak = weak_from_this(); auto callbacks = [OCTPIPStackCallbacks new];
  callbacks.checkIdentity = ^BOOL(OCTPIPStackIdentity* identity) { auto self = weak.lock(); return self && self->impl_->HasIdentity(identity); };
  callbacks.checkHost = ^BOOL(NSString* host, NSString* generation, NSWindow* window) { auto self = weak.lock(); return self && self->impl_->hosts && self->impl_->hosts->HasHost(host,generation,window); };
  callbacks.shouldShowGroup = ^BOOL(NSString*) { auto self = weak.lock(); return self && !self->impl_->stopped; };
  callbacks.focusRequested = ^(OCTPIPStackIdentity* identity) { if (auto self = weak.lock()) self->impl_->SendFocus(identity); };
  callbacks.maxDisplaySizeRequested = ^(CGFloat size) {
    if (auto self = weak.lock()) for (const auto& [key,entry] : self->impl_->receivers) {
      if (entry.channel->closed) continue;
      id<OCTPIPPublisher> peer = [entry.channel->connection remoteObjectProxyWithErrorHandler:^(NSError*) {}];
      [peer setMaxDisplaySize:size identity:IdentityObject(entry.source->identity) reply:^(NSError*) {}];
    }
  };
  callbacks.hideGroupsRequested = ^(NSArray<NSString*>* groups, BOOL all) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"hide-groups",@"groups":groups,@"all":@(all)}); };
  callbacks.returnToMainRequested = ^BOOL(OCTPIPStackIdentity* identity) { if (auto self = weak.lock()) { self->impl_->Emit(@{@"type":@"return-to-main",@"identity":IdentityObject(ReadIdentity(identity))}); return YES; } return NO; };
  callbacks.hostChanged = ^(NSString* host) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"host-changed",@"hostId":host ?: (id)NSNull.null}); };
  callbacks.anchorCommitted = ^(NSString* host, OCTPIPStackAlignment alignment) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"anchor-committed",@"hostId":host,@"alignment":@(alignment)}); };
  callbacks.restSizeChanged = ^(CGSize size) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"rest-size",@"width":@(size.width),@"height":@(size.height)}); };
  callbacks.lookTargetChanged = ^(CGPoint point, BOOL available) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"look-target",@"x":@(point.x),@"y":@(point.y),@"available":@(available)}); };
  callbacks.interrupted = ^(OCTPIPStackIdentity* identity, NSError*) { if (auto self = weak.lock()) self->impl_->Emit(@{@"type":@"presentation-interrupted",@"identity":identity ? IdentityObject(ReadIdentity(identity)) : (id)NSNull.null}); };
  NSError* error = nil; impl_->stack = [[OCTPIPStack alloc] initWithCallbacks:callbacks controlText:text placementAssetURL:asset error:&error];
  Require(impl_->stack && !error, "Native PiP stack construction failed"); impl_->hosts = std::make_shared<StackHostOwner>(impl_->stack);
}
ReceiverBootstrap::~ReceiverBootstrap() { impl_->on_event = {}; Stop(); }
NSDictionary* ReceiverBootstrap::BeginSource(NSDictionary* expected) {
  Main(); Require(!impl_->stopped, "Native PiP main owner was closed");
  Keys(expected,{@"requestId",@"scope",@"source",@"sourceWindowId",@"groupId",@"batchId",@"timeoutMs"});
  Scope(expected[@"scope"]); Name(expected[@"sourceWindowId"]); Name(expected[@"groupId"]); Name(expected[@"batchId"]);
  Keys(expected[@"source"],{@"tool",@"correlation_id",@"source_movement_seq",@"source_artifact"}); Name(expected[@"source"][@"tool"]); Name(expected[@"source"][@"correlation_id"]); Number(expected[@"source"][@"source_movement_seq"]);
  auto request = Name(expected[@"requestId"]); Require(!impl_->pending.contains(request) && impl_->pending.size() < kMaxBindings, "Native PiP request is duplicated or inventory is full");
  auto item = std::make_shared<Impl::Pending>(); item->request = request; item->nonce = RandomHex(); item->deadline = Deadline(Number(expected[@"timeoutMs"],600000));
  NSError* error = nil; NSData* copy = [NSJSONSerialization dataWithJSONObject:expected options:0 error:&error]; Require(!error, "Native PiP projected source cannot be retained"); item->expected = Parse(copy);
  impl_->pending.emplace(request,item); auto weak = weak_from_this();
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW,NanosecondsLeft(item->deadline)),dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) if (self->impl_->Current(item) && item->phase != Impl::Pending::Phase::Active && item->phase != Impl::Pending::Phase::Ending) self->impl_->Fail(item,@"source-timeout");
  });
  return @{@"requestId":Text(request),@"receiverPid":@(getpid()),@"receiverNonce":item->nonce,@"receiverInstanceNonce":impl_->instance_nonce};
}
void ReceiverBootstrap::EmbedSource(NSString* request, NSString* view, NSString* generation,
                                   NSString* host, NSString* host_generation, NSWindow* window, CGRect rect) {
  Main();auto found=impl_->pending.find(Name(request));
  Require(!impl_->stopped&&found!=impl_->pending.end(),"Embedded application has no current authenticated source");
  auto item=found->second;Name(view);Name(generation);Name(host);Name(host_generation);
  Require(impl_->Current(item)&&item->phase==Impl::Pending::Phase::Active&&!item->completed
          &&impl_->hosts->HasHost(host,host_generation,window),"Embedded application lost its active source or native host");
  NSError* error=nil;
  auto identity=[[OCTPIPStackIdentity alloc] initWithHostGeneration:Text(item->identity.host_generation)
      presentationID:Text(item->identity.presentation_id) streamGeneration:item->identity.stream_generation];
  Require([impl_->stack embedItem:identity viewID:view generation:generation hostID:host
           hostGeneration:host_generation window:window contentRect:rect error:&error]&&!error,"Embedded application source was refused");
}
void ReceiverBootstrap::DetachEmbeddedSource(NSString* view, NSString* generation) {
  Main();Name(view);Name(generation);if(!impl_->stopped)[impl_->stack detachEmbeddedView:view generation:generation];
}
void ReceiverBootstrap::HideEmbeddedSource(NSString* view, NSString* generation) {
  Main();Name(view);Name(generation);if(!impl_->stopped)[impl_->stack hideEmbeddedView:view generation:generation];
}
void ReceiverBootstrap::InstallSource(NSString* request, NSData* bytes, NSData* key) {
  Main(); auto found = impl_->pending.find(Name(request)); Require(found != impl_->pending.end(), "Native PiP request was not prepared"); auto item = found->second;
  try {
    Require(item->phase == Impl::Pending::Phase::AwaitingPrivateResponse && [key isKindOfClass:NSData.class] && key.length == 32, "Native PiP source install is duplicated or key size differs");
    NSDictionary* value = Parse(bytes);
    Keys(value,{@"schema_version",@"host_generation",@"bootstrap_id",@"request_id",@"receiver_nonce",@"receiver_instance_nonce",@"scope",@"source",@"presentation_id",@"stream_generation",@"source_window_id",@"native_window_incarnation",@"native_cg_window_id",@"receiver_pid",@"producer_pid",@"expires_at_mach_continuous_ticks"});
    Scope(value[@"scope"]); Hex(value[@"bootstrap_id"],32); Hex(value[@"receiver_nonce"],64); Hex(value[@"receiver_instance_nonce"],64);
    Require(Number(value[@"schema_version"]) == 1 && Name(value[@"request_id"]) == item->request && [value[@"receiver_nonce"] isEqual:item->nonce] && [value[@"receiver_instance_nonce"] isEqual:impl_->instance_nonce] && [value[@"scope"] isEqual:item->expected[@"scope"]] && [value[@"source"] isEqual:item->expected[@"source"]] && [value[@"source_window_id"] isEqual:item->expected[@"sourceWindowId"]] && [value[@"host_generation"] isEqual:value[@"scope"][@"host_generation"]] && Number(value[@"receiver_pid"],INT32_MAX) == static_cast<uint64_t>(getpid()), "Native PiP binding does not match the current projected request");
    Decimal(value[@"native_window_incarnation"]); Number(value[@"native_cg_window_id"],UINT32_MAX);
    uint64_t expiry = Decimal(value[@"expires_at_mach_continuous_ticks"]); NanosecondsLeft(expiry);
    item->deadline = std::min(item->deadline,expiry); NanosecondsLeft(item->deadline); item->identity = {Name(value[@"host_generation"]),Name(value[@"presentation_id"]),Number(value[@"stream_generation"])};
    Require(!impl_->receivers.contains(Key(item->identity)), "Native PiP presentation identity is already installed");
    pid_t producer = static_cast<pid_t>(Number(value[@"producer_pid"],INT32_MAX)); Require(producer != getpid(), "Native PiP producer cannot be the receiver");
    NSError* permission_error=nil;
    auto permission=[[OCTAutomationPermission alloc] initWithRecipientProcessID:producer identifier:[@"automation:" stringByAppendingString:RandomHex()] error:&permission_error];
    Require(permission && !permission_error,"Native Automation process identity is unavailable");
    for(const auto& [request,source]:impl_->permissions)if([[source.permission recipientIdentity] isEqual:[permission recipientIdentity]] && [source.permission isCurrent]){permission=source.permission;break;}
    Require(impl_->permissions.size()<kMaxBindings,"Native permission source inventory is full");
    impl_->permissions.emplace(item->request,Impl::PermissionSource{permission,value[@"scope"],value[@"source_window_id"]});
    auto weak = weak_from_this();
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,NanosecondsLeft(item->deadline)),dispatch_get_main_queue(), ^{
      if (auto self = weak.lock()) if (self->impl_->Current(item) && item->phase != Impl::Pending::Phase::Active && item->phase != Impl::Pending::Phase::Ending) self->impl_->Fail(item,@"source-timeout");
    });
    item->binding = value; item->bytes = [bytes copy]; item->bootstrap = [value[@"bootstrap_id"] copy]; std::memcpy(item->key.bytes.data(),key.bytes,32);
    auto channel_key = std::make_pair(item->identity.host_generation,producer); auto channel_it = impl_->channels.find(channel_key); bool fresh = channel_it == impl_->channels.end();
    auto channel = fresh ? std::make_shared<Impl::Channel>() : channel_it->second;
    Require(!channel->closed && channel->queue.size() < kMaxBindings, "Native PiP channel is unavailable");
    if (fresh) { channel->host = item->identity.host_generation; channel->producer = producer; impl_->channels.emplace(channel_key,channel); }
    item->channel = channel; item->phase = Impl::Pending::Phase::Queued; channel->queue.push_back(item);
    if (fresh) impl_->RendezvousStart(channel,item); else impl_->Authenticate(channel);
  } catch (...) { impl_->Fail(item); throw; }
}
void ReceiverBootstrap::CancelSource(NSString* request) { Main(); auto key=Name(request);impl_->RemovePermission(key);auto found=impl_->pending.find(key);if(found!=impl_->pending.end())impl_->Fail(found->second,@"source-cancelled"); }
NSDictionary* ReceiverBootstrap::PrepareAutomationPermission(NSString* request) {
  Main();auto found=impl_->permissions.find(Name(request));Require(!impl_->stopped&&found!=impl_->permissions.end()&&[found->second.permission isCurrent],"Native Automation source is no longer available");return [found->second.permission snapshot];
}
void ReceiverBootstrap::BeginAutomationPermission(NSString* request, NSString* operation_id, NSString* operation, uint64_t timeout_ms) {
  Main();auto request_key=Name(request);Name(operation_id);Name(operation);auto found=impl_->permissions.find(request_key);
  Require(!impl_->stopped&&found!=impl_->permissions.end(),"Native Automation request source was withdrawn");
  auto permission=found->second.permission;__weak OCTAutomationPermission* permission_weak=permission;auto weak=weak_from_this();NSError* error=nil;
  BOOL accepted=[permission begin:operation operationID:operation_id timeoutMilliseconds:timeout_ms completion:^(NSDictionary* result,NSError* failure){
    if(auto self=weak.lock()){
      auto current=self->impl_->permissions.find(request_key);if(self->impl_->stopped||current==self->impl_->permissions.end()||current->second.permission!=permission_weak)return;
      self->impl_->Emit(@{@"type":@"automation-permission-result",@"requestId":request,@"operationId":operation_id,@"requirement":result?:NSNull.null,@"error":failure.localizedDescription?: (id)NSNull.null});
    }
  } error:&error];Require(accepted&&!error,"The native Automation operation is not currently available");
}
void ReceiverBootstrap::CancelAutomationSettings(NSString* request, NSString* operation_id) {
  Main();auto request_key=Name(request);Name(operation_id);auto found=impl_->permissions.find(request_key);
  if(found!=impl_->permissions.end())[found->second.permission cancelSettings:operation_id];
}
void ReceiverBootstrap::UpsertBrowserImage(NSDictionary* object, NSString* group, NSString* batch, uint64_t sequence, NSData* bytes) {
  Main(); Require(!impl_->stopped, "Native PiP main owner was closed");
  const auto identity = ReadIdentity(object); const auto key = Key(identity);
  const auto group_name = Name(group), batch_name = Name(batch);
  Require(identity.presentation_id.starts_with("browser:") && sequence > 0 && sequence <= kMaxSequence,
          "Browser PiP identity or source sequence is invalid");
  Require([bytes isKindOfClass:NSData.class] && bytes.length > 0 && bytes.length <= 100000000,
          "Browser PiP image exceeds the browser screenshot bound");
  Require(!impl_->receivers.contains(key), "Browser image cannot replace a native app source");
  std::array<uint8_t, CC_SHA256_DIGEST_LENGTH> digest{};
  CC_SHA256(bytes.bytes, static_cast<CC_LONG>(bytes.length), digest.data());
  auto found = impl_->browser_images.find(key);
  if (found != impl_->browser_images.end()) {
    Require(found->second.group == group_name && found->second.batch == batch_name,
            "Browser image cannot change its exact group or batch");
    Require(sequence >= found->second.sequence, "Browser image source sequence is stale");
    if (sequence == found->second.sequence) {
      Require(digest == found->second.digest, "Browser image changed under the same source sequence");
      return;
    }
  } else Require(impl_->browser_images.size() + impl_->pending.size() < kMaxBindings,
                 "Native PiP presentation inventory is full");
  NSImage* image = [[NSImage alloc] initWithData:bytes];
  Require(image && std::isfinite(image.size.width) && std::isfinite(image.size.height) && image.size.width > 0 && image.size.height > 0
          && [image CGImageForProposedRect:NULL context:nil hints:nil], "Browser PiP screenshot cannot be decoded");
  auto native_identity = [[OCTPIPStackIdentity alloc] initWithHostGeneration:Text(identity.host_generation) presentationID:Text(identity.presentation_id) streamGeneration:identity.stream_generation];
  NSError* error = nil;
  if (found != impl_->browser_images.end()) {
    Require([impl_->stack updateImage:image identity:native_identity error:&error] && !error,
            "Browser PiP image update was rejected");
    found->second.sequence = sequence; found->second.digest = digest;
  } else {
    impl_->browser_images.emplace(key,Impl::BrowserImage{identity,group_name,batch_name,sequence,digest});
    if (![impl_->stack publishImageItem:native_identity image:image groupID:group batchID:batch animated:YES error:&error] || error) {
      impl_->browser_images.erase(key); throw std::runtime_error("Browser PiP image publication was rejected");
    }
  }
  impl_->Emit(@{@"type":@"browser-image-published",@"identity":IdentityObject(identity),@"sourceMovementSeq":@(sequence)});
}
void ReceiverBootstrap::InvalidateBrowserImage(NSDictionary* object) {
  Main(); const auto identity = ReadIdentity(object); const auto key = Key(identity);
  auto found = impl_->browser_images.find(key); if (found == impl_->browser_images.end()) return;
  auto native_identity = [[OCTPIPStackIdentity alloc] initWithHostGeneration:Text(identity.host_generation) presentationID:Text(identity.presentation_id) streamGeneration:identity.stream_generation];
  [impl_->stack removeItem:native_identity animated:YES completion:nil]; impl_->browser_images.erase(found);
  impl_->Emit(@{@"type":@"browser-image-invalidated",@"identity":IdentityObject(identity)});
}
void ReceiverBootstrap::RevokeSource(NSDictionary* scope, NSString* window) {
  Main(); Scope(scope); if (window) Name(window); std::vector<std::shared_ptr<Impl::Pending>> remove;
  for (const auto& [id,item] : impl_->pending) if ([item->expected[@"scope"] isEqual:scope] && (!window || [item->expected[@"sourceWindowId"] isEqual:window])) remove.push_back(item);
  for (auto& item : remove) impl_->Fail(item,@"source-withdrawn");
  std::vector<std::string> permission_remove;
  for(const auto& [request,source]:impl_->permissions)if([source.scope isEqual:scope]&&(!window||[source.window isEqual:window]))permission_remove.push_back(request);
  for(const auto& request:permission_remove)impl_->RemovePermission(request);
}
StackHostOwner& ReceiverBootstrap::Hosts() { Main(); Require(!impl_->stopped && impl_->hosts, "Native PiP window host is unavailable"); return *impl_->hosts; }
OCTPIPStack* ReceiverBootstrap::Stack() { Main(); Require(!impl_->stopped, "Native PiP stack is closed"); return impl_->stack; }
NSDictionary* ReceiverBootstrap::Status() const { Main(); return @{@"stopped":@(impl_->stopped),@"channels":@(impl_->channels.size()),@"sources":@(impl_->receivers.size()),@"browserImages":@(impl_->browser_images.size()),@"pending":@(impl_->pending.size()-impl_->receivers.size())}; }
void ReceiverBootstrap::Stop() {
  Main(); if (impl_->stopped) return; impl_->stopped = true;
  while (!impl_->channels.empty()) impl_->CloseChannel(impl_->channels.begin()->second);
  for(const auto& [request,source]:impl_->permissions)[source.permission invalidate];impl_->permissions.clear();
  for (auto& [id,item] : impl_->pending) { item->key.Clear(); item->phase = Impl::Pending::Phase::Cancelled; } impl_->pending.clear();
  if (impl_->hosts) impl_->hosts->Close(); impl_->hosts.reset(); [impl_->stack close]; impl_->browser_images.clear(); impl_->stack = nil;
}
void ReceiverBootstrap::Dispatch(NSXPCConnection* sender, NSDictionary* object, std::function<void(Receiver&)> action, void (^reply)(NSError*)) {
  Main(); std::shared_ptr<Impl::Pending> source;
  try {
    auto identity = ReadIdentity(object); auto found = impl_->receivers.find(Key(identity));
    Require(!impl_->stopped && found != impl_->receivers.end() && !found->second.channel->closed && found->second.channel->connection == sender && sender.processIdentifier == found->second.channel->producer && (found->second.source->phase == Impl::Pending::Phase::Active || found->second.source->phase == Impl::Pending::Phase::Activating), "Native PiP source is not admitted on this actual peer");
    source = found->second.source; auto receiver = found->second.receiver;
    action(*receiver); reply(nil);
  } catch (...) {
    if (source) {
      auto found = impl_->receivers.find(Key(source->identity));
      if (found != impl_->receivers.end() && found->second.source == source && source->phase != Impl::Pending::Phase::Ending && !found->second.receiver->authenticated()) impl_->Fail(source);
    }
    reply(Failure(@"Native PiP source operation was rejected"));
  }
}
void ReceiverBootstrap::CompleteGroup(NSString* group) {
  Main(); const auto group_name = Name(group); auto weak = weak_from_this();
  std::vector<Identity> browser_images;
  for (const auto& [key,image] : impl_->browser_images) if (image.group == group_name) browser_images.push_back(image.identity);
  for (const auto& identity : browser_images) InvalidateBrowserImage(IdentityObject(identity));
  for (auto& [identity,entry] : impl_->receivers) {
    auto item = entry.source;
    if (item->phase != Impl::Pending::Phase::Active || item->completed || ![item->expected[@"groupId"] isEqual:group]) continue;
    entry.receiver->ShowCompletion(); item->completed = true;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,30000000000LL),dispatch_get_main_queue(), ^{
      if (auto self = weak.lock()) if (self->impl_->Current(item) && item->completed && item->phase == Impl::Pending::Phase::Active) self->impl_->End(item);
    });
  }
}
void ReceiverBootstrap::ClearCompletedGroup(NSString* group) {
  Main(); Name(group); std::vector<std::shared_ptr<Impl::Pending>> items;
  for (auto& [identity,entry] : impl_->receivers) if (entry.source->completed && [entry.source->expected[@"groupId"] isEqual:group]) items.push_back(entry.source);
  for (auto& item : items) impl_->End(item);
}
void ReceiverBootstrap::WillEnd(NSXPCConnection* sender, NSDictionary* object, void (^reply)(NSError*)) {
  Dispatch(sender,object,[&](Receiver&) { auto found = impl_->receivers.find(Key(ReadIdentity(object))); impl_->End(found->second.source); },reply);
}
void ReceiverBootstrap::Invalidate(NSXPCConnection* sender, NSDictionary* object, NSString* reason, void (^reply)(NSError*)) {
  Main();
  try {
    auto identity = ReadIdentity(object); auto found = impl_->receivers.find(Key(identity));
    Require(!impl_->stopped && found != impl_->receivers.end() && found->second.channel->connection == sender && sender.processIdentifier == found->second.channel->producer && !found->second.channel->closed,
            "Native PiP invalidation requires the exact admitted peer/source");
    Require([reason isEqual:@"ended"] || [reason isEqual:@"withdrawn"], "Native PiP invalidation reason is unknown");
    auto item = found->second.source;
    if ([reason isEqual:@"withdrawn"]) { impl_->Fail(item,@"source-withdrawn"); reply(nil); }
    else if (item->completed && item->phase == Impl::Pending::Phase::Active) {
      Require(item->invalidation_replies.count < 1024, "Native PiP pending invalidation reply bound exceeded"); [item->invalidation_replies addObject:[reply copy]];
    } else { impl_->End(item); reply(nil); }
  } catch (...) { reply(Failure(@"Native PiP invalidation was rejected")); }
}
}

namespace {
struct NativeReplyPermit {
  std::shared_ptr<std::atomic<size_t>> count;
  std::atomic<bool> called{false};
  ~NativeReplyPermit() { count->fetch_sub(1); }
};
void (^BoundedReply(const std::shared_ptr<std::atomic<size_t>>& count, void (^reply)(NSError*)))(NSError*) {
  if (!count || count->fetch_add(1) >= 1024) {
    if (count) count->fetch_sub(1);
    reply(octopus::pip::Failure(@"Native PiP operation queue is full")); return nil;
  }
  auto permit = std::make_shared<NativeReplyPermit>(); permit->count = count;
  return [^(NSError* error) { if (!permit->called.exchange(true)) reply(error); } copy];
}
}
@implementation OCTPIPReceiverExport
- (void)publishPresentation:(NSDictionary*)identity contextID:(uint32_t)context width:(double)width height:(double)height reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  dispatch_async(dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) self->Dispatch(sender,identity,[=](auto& receiver) { receiver.Publish(sender,octopus::pip::ReadIdentity(identity),context,CGSizeMake(width,height)); },reply);
    else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
  });
}
- (void)preparePresentation:(NSDictionary*)identity operationID:(uint64_t)operation kind:(NSString*)kind contextID:(uint32_t)context width:(double)width height:(double)height fence:(id)payload reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  try {
    auto right = std::make_shared<octopus::pip::SendRight>(octopus::pip::SendRight::CopyFence((xpc_object_t)payload));
    dispatch_async(dispatch_get_main_queue(), ^{
      if (auto self = weak.lock()) self->Dispatch(sender,identity,[=](auto& receiver) {
        octopus::pip::OperationKind operation_kind;
        if ([kind isEqual:@"resize"]) operation_kind = octopus::pip::OperationKind::Resize;
        else if ([kind isEqual:@"resize-in-progress"]) operation_kind = octopus::pip::OperationKind::ResizeInProgress;
        else { octopus::pip::Require([kind isEqual:@"replace-context"],"Unknown PiP operation kind"); operation_kind = octopus::pip::OperationKind::ReplaceContext; }
        receiver.Prepare(sender,octopus::pip::ReadIdentity(identity),operation,operation_kind,context,CGSizeMake(width,height),std::move(*right));
      },reply); else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
    });
  } catch (...) { reply(octopus::pip::Failure(@"Native PiP fence was rejected")); }
}
- (void)completePresentation:(NSDictionary*)identity operationID:(uint64_t)operation reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  dispatch_async(dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) self->Dispatch(sender,identity,[=](auto& receiver) { receiver.Complete(sender,octopus::pip::ReadIdentity(identity),operation); },reply);
    else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
  });
}
- (void)willEndPresentation:(NSDictionary*)identity reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  dispatch_async(dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) self->WillEnd(sender,identity,reply); else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
  });
}
- (void)invalidatePresentation:(NSDictionary*)identity reason:(NSString*)reason reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  dispatch_async(dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) self->Invalidate(sender,identity,reason,reply); else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
  });
}
- (void)noteInteraction:(NSDictionary*)identity reply:(void (^)(NSError*))reply {
  reply = BoundedReply(in_flight,reply); if (!reply) return;
  NSXPCConnection* sender = NSXPCConnection.currentConnection; auto weak = owner;
  dispatch_async(dispatch_get_main_queue(), ^{
    if (auto self = weak.lock()) self->Dispatch(sender,identity,[=](auto&) {
      auto value = octopus::pip::ReadIdentity(identity);
      [self->Stack() noteInteraction:[[OCTPIPStackIdentity alloc] initWithHostGeneration:octopus::pip::Text(value.host_generation) presentationID:octopus::pip::Text(value.presentation_id) streamGeneration:value.stream_generation]];
    },reply); else reply(octopus::pip::Failure(@"Native PiP owner was closed"));
  });
}
@end
