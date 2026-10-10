#include "receiver.h"
#import "stack.h"

#include <cmath>
#include <dlfcn.h>
#include <map>
#include <stdexcept>
#include <utility>

@protocol OPPIPContext
+ (id)contextWithCGSConnection:(uint32_t)connection options:(NSDictionary*)options;
- (uint32_t)contextId;
- (void)setLayer:(CALayer*)layer;
- (void)setFencePort:(mach_port_t)port;
@end

@protocol OPPIPLayerHost
- (void)setContextId:(uint32_t)context;
@end

@interface OPPIPAttachment : NSObject <OCTPIPStackAttachment> {
  std::shared_ptr<octopus::pip::LayerAttachment> attachment_;
}
- (instancetype)initWithAttachment:(std::shared_ptr<octopus::pip::LayerAttachment>)attachment;
@end

@implementation OPPIPAttachment
- (instancetype)initWithAttachment:(std::shared_ptr<octopus::pip::LayerAttachment>)attachment {
  if ((self = [super init])) attachment_ = std::move(attachment);
  return self;
}
- (BOOL)setFrameInCurrentTransaction:(CGRect)frame scaleFromCommittedSize:(BOOL)scale
                            visible:(BOOL)visible error:(NSError**)error {
  try {
    attachment_->SetFrameInCurrentTransaction(frame, scale, visible);
    return YES;
  } catch (const std::exception& failure) {
    if (error) *error = [NSError errorWithDomain:@"OctopusNativePIP" code:1 userInfo:@{
      NSLocalizedDescriptionKey: [NSString stringWithUTF8String:failure.what()]}];
    return NO;
  }
}
- (void)detach { attachment_->Detach(); }
@end

namespace octopus::pip {
namespace {
void Require(bool value, const char* message) {
  if (!value) throw std::runtime_error(message);
}
void MainThread() { Require([NSThread isMainThread], "PiP receiver requires the main thread"); }
bool Positive(CGSize size) {
  return std::isfinite(size.width) && std::isfinite(size.height) &&
         size.width > 0 && size.height > 0;
}
bool Frame(CGRect frame) {
  return std::isfinite(frame.origin.x) && std::isfinite(frame.origin.y) && Positive(frame.size);
}
bool OptionalRect(CGRect rect) {
  return std::isfinite(rect.origin.x) && std::isfinite(rect.origin.y) &&
         std::isfinite(rect.size.width) && std::isfinite(rect.size.height) &&
         rect.size.width >= 0 && rect.size.height >= 0;
}
bool Name(const std::string& name) {
  return !name.empty() && name.size() <= kMaxMetadataBytes && name.find('\0') == std::string::npos;
}
bool Sequence(uint64_t value) { return value && value <= kMaxSequence; }
void ValidateIdentity(const Identity& identity) {
  Require(Name(identity.host_generation) && Name(identity.presentation_id) &&
          Sequence(identity.stream_generation), "Invalid PiP native identity");
}
class Transaction {
 public:
  Transaction() { [CATransaction begin]; [CATransaction setDisableActions:YES]; }
  ~Transaction() { [CATransaction commit]; }
};
CALayer<OPPIPLayerHost>* HostLayer(uint32_t context_id) {
  Class cls = NSClassFromString(@"CALayerHost");
  Require(cls && [cls isSubclassOfClass:[CALayer class]] &&
          [cls instancesRespondToSelector:@selector(setContextId:)], "Native CALayerHost is unavailable");
  CALayer<OPPIPLayerHost>* layer = [[cls alloc] init];
  Require(layer != nil, "Native CALayerHost creation failed");
  [layer setContextId:context_id];
  return layer;
}
id<OPPIPContext> Context() {
  Class cls = NSClassFromString(@"CAContext");
  auto connection = reinterpret_cast<uint32_t (*)()>(dlsym(RTLD_DEFAULT, "CGSMainConnectionID"));
  Require(cls && connection && [cls respondsToSelector:@selector(contextWithCGSConnection:options:)],
          "Native remote layer context is unavailable");
  id<OPPIPContext> context = [(Class<OPPIPContext>)cls contextWithCGSConnection:connection() options:@{}];
  Require(context && [(id)context respondsToSelector:@selector(contextId)] &&
          [(id)context respondsToSelector:@selector(setLayer:)] &&
          [(id)context respondsToSelector:@selector(setFencePort:)] && [context contextId],
          "Native remote layer context lacks the required fence capability");
  return context;
}
}

SendRight SendRight::CopyFence(xpc_object_t payload) {
  Require(payload && xpc_get_type(payload) == XPC_TYPE_DICTIONARY, "Fence payload must be a native XPC dictionary");
  auto copy = reinterpret_cast<mach_port_t (*)(xpc_object_t, const char*)>(
      dlsym(RTLD_DEFAULT, "xpc_dictionary_copy_mach_send"));
  Require(copy != nullptr, "Native XPC Mach-send transport is unavailable");
  SendRight right(copy(payload, "fence"));
  Require(MACH_PORT_VALID(right.port_), "Fence payload contains no transferred Mach send right");
  mach_port_type_t type = 0;
  Require(mach_port_type(mach_task_self(), right.port_, &type) == KERN_SUCCESS &&
          (type & MACH_PORT_TYPE_SEND), "Fence payload is not a live send right");
  return right;
}
SendRight::SendRight(SendRight&& other) noexcept : port_(std::exchange(other.port_, MACH_PORT_NULL)) {}
SendRight& SendRight::operator=(SendRight&& other) noexcept {
  if (this != &other) {
    if (MACH_PORT_VALID(port_)) mach_port_deallocate(mach_task_self(), port_);
    port_ = std::exchange(other.port_, MACH_PORT_NULL);
  }
  return *this;
}
SendRight::~SendRight() {
  if (MACH_PORT_VALID(port_)) mach_port_deallocate(mach_task_self(), port_);
}

void OperationState::Validate(uint64_t operation, OperationKind kind, uint32_t context, CGSize size) const {
  Require(Sequence(operation) && completed < kMaxSequence && operation == completed + 1 && !pending,
          "PiP operation is not the next unoccupied sequence");
  Require(context && Positive(size), "PiP operation has invalid context or size");
  Require(kind == OperationKind::Resize || kind == OperationKind::ResizeInProgress ||
          kind == OperationKind::ReplaceContext, "Unknown PiP operation");
  Require(kind == OperationKind::ReplaceContext ? context != context_id : context == context_id,
          "PiP operation context does not match its operation kind");
}
void OperationState::Prepared(uint64_t operation, OperationKind kind, uint32_t context, CGSize size) {
  Validate(operation, kind, context, size);
  if (kind != OperationKind::ResizeInProgress) { context_id = context; source_size = size; }
  pending = operation;
}
void OperationState::Complete(uint64_t operation) {
  Require(Sequence(operation) && pending == operation, "PiP completion does not identify the pending operation");
  completed = operation;
  pending = 0;
}

struct LayerAttachment::Impl {
  CALayer* __strong root = nil;
  CALayer<OPPIPLayerHost>* __strong receiver_host = nil;
  id<OPPIPContext> __strong receiver_context = nil;
  CALayer* __strong receiver_root = nil;
  CALayer<OPPIPLayerHost>* __strong producer_host = nil;
  CGSize committed_size = CGSizeZero;
  ~Impl() { [receiver_context setLayer:nil]; [root removeFromSuperlayer]; }

  void Geometry(bool scale) {
    const CGRect bounds = root.bounds;
    receiver_host.frame = bounds;
    receiver_root.frame = bounds;
    if (!producer_host) return;
    if (scale && Positive(committed_size) && Positive(bounds.size)) {
      producer_host.bounds = CGRectMake(0, 0, committed_size.width, committed_size.height);
      producer_host.position = CGPointMake(CGRectGetMidX(bounds), CGRectGetMidY(bounds));
      producer_host.transform = CATransform3DMakeScale(bounds.size.width / committed_size.width,
                                                      bounds.size.height / committed_size.height, 1);
    } else {
      producer_host.transform = CATransform3DIdentity;
      producer_host.frame = receiver_root.bounds;
    }
  }
};

LayerAttachment::LayerAttachment(CALayer* parent, CGRect frame, uint32_t context_id)
    : impl_(std::make_unique<Impl>()) {
  MainThread();
  Require(parent && Frame(frame) && context_id, "Invalid native PiP attachment");
  impl_->receiver_context = Context();
  impl_->receiver_host = HostLayer([impl_->receiver_context contextId]);
  impl_->producer_host = HostLayer(context_id);
  impl_->root = [CALayer layer];
  impl_->root.frame = frame;
  impl_->root.masksToBounds = YES;
  impl_->receiver_root = [CALayer layer];
  impl_->receiver_root.masksToBounds = YES;
  impl_->committed_size = frame.size;
  {
    Transaction transaction;
    [impl_->root addSublayer:impl_->receiver_host];
    [impl_->receiver_context setLayer:impl_->receiver_root];
    [impl_->receiver_root addSublayer:impl_->producer_host];
    impl_->Geometry(false);
    [parent addSublayer:impl_->root];
  }
  [CATransaction flush];
}
LayerAttachment::~LayerAttachment() { Detach(); }
CALayer* LayerAttachment::layer() const { return impl_->root; }

void LayerAttachment::Prepare(OperationKind kind, uint32_t context_id, CGSize display_size,
                              const SendRight& fence) {
  MainThread();
  Require(context_id && (kind == OperationKind::Resize || kind == OperationKind::ResizeInProgress ||
          kind == OperationKind::ReplaceContext) && impl_->root && impl_->producer_host &&
          Positive(display_size) && MACH_PORT_VALID(fence.get()),
          "PiP attachment is not available for fenced preparation");
  if (kind == OperationKind::ResizeInProgress) {
    [impl_->receiver_context setFencePort:fence.get()];
    return;
  }
  CALayer<OPPIPLayerHost>* replacement = kind == OperationKind::ReplaceContext ? HostLayer(context_id) : nil;
  [impl_->receiver_context setFencePort:fence.get()];
  {
    Transaction transaction;
    CGRect frame = impl_->root.frame;
    frame.size = display_size;
    impl_->root.frame = frame;
    if (replacement) {
      [impl_->receiver_root addSublayer:replacement];
      [impl_->producer_host removeFromSuperlayer];
      impl_->producer_host = replacement;
    }
    impl_->committed_size = display_size;
    impl_->Geometry(false);
  }
  [CATransaction flush];
}
void LayerAttachment::SetFrame(CGRect frame, bool scale, bool visible) {
  MainThread();
  Transaction transaction;
  SetFrameInCurrentTransaction(frame, scale, visible);
}
void LayerAttachment::SetFrameInCurrentTransaction(CGRect frame, bool scale, bool visible) {
  MainThread();
  Require(impl_->root && Frame(frame), "PiP attachment frame is unavailable or invalid");
  impl_->root.frame = frame;
  impl_->root.hidden = !visible;
  impl_->Geometry(scale);
}
void LayerAttachment::Clear() {
  MainThread();
  [impl_->producer_host removeAllAnimations];
  impl_->producer_host.transform = CATransform3DIdentity;
  [impl_->producer_host removeFromSuperlayer];
  impl_->producer_host = nil;
  impl_->committed_size = CGSizeZero;
}
void LayerAttachment::Detach() {
  if (!impl_->root) return;
  MainThread();
  Transaction transaction;
  Clear();
  [impl_->receiver_context setLayer:nil];
  [impl_->receiver_host removeAllAnimations];
  [impl_->receiver_root removeAllAnimations];
  [impl_->root removeAllAnimations];
  impl_->root.hidden = YES;
  [impl_->root removeFromSuperlayer];
  impl_->root = nil;
  impl_->receiver_host = nil;
  impl_->receiver_root = nil;
  impl_->receiver_context = nil;
}

id<OCTPIPStackAttachment> WrapAttachment(std::shared_ptr<LayerAttachment> attachment) {
  MainThread(); Require(attachment != nullptr, "Cannot wrap an absent native PiP attachment");
  return [[OPPIPAttachment alloc] initWithAttachment:std::move(attachment)];
}

struct StackHostOwner::Impl {
  struct Host {
    NSView* __weak view = nil;
    NSWindow* __weak window = nil;
    id __strong observer = nil;
    NSString* __strong generation = nil;
  };
  OCTPIPStack* __strong stack = nil;
  std::map<std::string, Host> hosts;
  bool closed = false;
};
StackHostOwner::StackHostOwner(OCTPIPStack* stack) : impl_(std::make_unique<Impl>()) {
  MainThread(); Require(stack != nil, "PiP host owner requires its actual shared native stack");
  impl_->stack = stack;
}
StackHostOwner::~StackHostOwner() { Close(); }
bool StackHostOwner::OwnsStack(OCTPIPStack* stack) const {
  MainThread(); return !impl_->closed && stack && impl_->stack == stack;
}
bool StackHostOwner::HasHost(NSString* host_id, NSString* generation, NSWindow* window) const {
  MainThread();
  if (impl_->closed || !host_id || !generation || !window) return false;
  const auto host = impl_->hosts.find(host_id.UTF8String);
  return host != impl_->hosts.end() && host->second.window == window && host->second.view &&
      host->second.view == window.contentView && [host->second.generation isEqualToString:generation];
}
void StackHostOwner::RegisterHost(const std::string& host_id, const std::string& generation,
                            const void* handle, CGRect anchor, CGRect passthrough,
                            NSArray<OCTPIPStackHostAnchor*>* anchors, OCTPIPStackScope scope,
                            OCTPIPStackHostRole role, bool home_available, bool animated) {
  MainThread();
  Require(!weak_from_this().expired() && !impl_->closed && Name(host_id) && Name(generation) && handle &&
          OptionalRect(anchor) && OptionalRect(passthrough) && anchors &&
          (scope == OCTPIPStackActive || scope == OCTPIPStackAll) &&
          (role == OCTPIPStackMain || role == OCTPIPStackAccessory), "Invalid PiP native host registration");
  NSView* view = nil; NSWindow* window = nil;
  for (NSWindow* candidate in NSApp.windows) {
    if ((__bridge const void*)candidate.contentView == handle) { view = candidate.contentView; window = candidate; break; }
  }
  Require(view && window, "PiP handle does not identify an actual current application window content view");
  const auto found = impl_->hosts.find(host_id);
  if (found != impl_->hosts.end()) Require(found->second.view == view && found->second.window == window &&
      [found->second.generation isEqualToString:[NSString stringWithUTF8String:generation.c_str()]],
      "PiP host identity cannot retarget another native window or generation");
  Require(found != impl_->hosts.end() || impl_->hosts.size() < 1024, "PiP native host inventory exceeds its bound");
  const bool added = found == impl_->hosts.end();
  if (added) {
    Impl::Host host; host.view = view; host.window = window;
    host.generation = [NSString stringWithUTF8String:generation.c_str()];
    const std::string retained_id = host_id, retained_generation = generation;
    auto weak = weak_from_this();
    host.observer = [[NSNotificationCenter defaultCenter] addObserverForName:NSWindowWillCloseNotification
        object:window queue:[NSOperationQueue mainQueue] usingBlock:^(NSNotification*) {
          if (auto owner = weak.lock()) owner->RemoveHost(retained_id, retained_generation);
        }];
    impl_->hosts.emplace(host_id, std::move(host));
  }
  NSError* error = nil;
  if (![impl_->stack registerHost:[NSString stringWithUTF8String:host_id.c_str()]
      generation:[NSString stringWithUTF8String:generation.c_str()] window:window anchorContentRect:anchor
      passthroughRect:passthrough anchors:anchors scope:scope role:role homeAvailable:home_available
      animated:animated spring:nil error:&error]) {
    if (added) RemoveHost(host_id, generation);
    throw std::runtime_error("Native PiP stack host registration failed");
  }
}
void StackHostOwner::RemoveHost(const std::string& host_id, const std::string& generation) {
  MainThread();
  const auto found = impl_->hosts.find(host_id);
  if (found == impl_->hosts.end() ||
      ![found->second.generation isEqualToString:[NSString stringWithUTF8String:generation.c_str()]]) return;
  Impl::Host host = std::move(found->second);
  impl_->hosts.erase(found);
  [[NSNotificationCenter defaultCenter] removeObserver:host.observer];
  [impl_->stack removeHost:[NSString stringWithUTF8String:host_id.c_str()] generation:host.generation];
}
void StackHostOwner::Close() {
  MainThread();
  if (impl_->closed) return;
  impl_->closed = true;
  auto hosts = std::move(impl_->hosts);
  for (auto& [id, host] : hosts) {
    [[NSNotificationCenter defaultCenter] removeObserver:host.observer];
    [impl_->stack removeHost:[NSString stringWithUTF8String:id.c_str()] generation:host.generation];
  }
  impl_->stack = nil;
}

struct Receiver::Impl {
  Identity expected;
  OnEvent on_event;
  NSXPCConnection* __strong peer = nil;
  OCTPIPStack* __strong stack = nil;
  OCTPIPStackIdentity* __strong stack_identity = nil;
  NSString* __strong group_id = nil;
  NSString* __strong batch_id = nil;
  std::weak_ptr<StackHostOwner> host_owner;
  std::shared_ptr<LayerAttachment> attachment;
  std::string attachment_id;
  uint64_t generation = 0;
  uint64_t last_generation = 0;
  OperationState operations;
  bool published = false;
  bool stopped = false;
};
Receiver::Receiver(Identity expected, OnEvent on_event) : impl_(std::make_unique<Impl>()) {
  MainThread(); ValidateIdentity(expected);
  impl_->expected = std::move(expected); impl_->on_event = std::move(on_event);
}
Receiver::~Receiver() { impl_->on_event = {}; Stop(); }
bool Receiver::authenticated() const { return !impl_->stopped && impl_->peer != nil; }
bool Receiver::published() const { return authenticated() && impl_->published; }
bool Receiver::MatchesIdentity(OCTPIPStackIdentity* identity) const {
  MainThread();
  return published() && [impl_->stack_identity isEqual:identity];
}
bool Receiver::HasHost(NSString* host_id, NSString* generation, NSWindow* window) const {
  MainThread();
  auto owner = impl_->host_owner.lock();
  return authenticated() && owner && owner->HasHost(host_id, generation, window);
}
void Receiver::InstallAuthenticatedConnection(NSXPCConnection* connection, const Identity& verified,
                                               OCTPIPStack* stack, std::shared_ptr<StackHostOwner> host_owner,
                                               NSString* group_id, NSString* batch_id) {
  MainThread();
  Require(!weak_from_this().expired() && !impl_->stopped && !impl_->peer && connection && connection.processIdentifier > 0 && stack &&
          host_owner && host_owner->OwnsStack(stack) && group_id.length && batch_id.length && verified == impl_->expected,
          "PiP bootstrap connection does not match the native receiver or exact client presentation owner");
  impl_->stack_identity = [[OCTPIPStackIdentity alloc]
      initWithHostGeneration:[NSString stringWithUTF8String:verified.host_generation.c_str()]
      presentationID:[NSString stringWithUTF8String:verified.presentation_id.c_str()]
      streamGeneration:verified.stream_generation];
  impl_->stack = stack; impl_->host_owner = host_owner;
  impl_->group_id = [group_id copy]; impl_->batch_id = [batch_id copy]; impl_->peer = connection;
}
void Receiver::CheckPeer(NSXPCConnection* sender, const Identity& identity) const {
  MainThread();
  auto host_owner = impl_->host_owner.lock();
  Require(authenticated() && host_owner && host_owner->OwnsStack(impl_->stack) &&
          sender == impl_->peer && identity == impl_->expected,
          "PiP publication requires its completed existing-authority native bootstrap");
}
void Receiver::Emit(const std::string& type, uint64_t operation) {
  auto callback = impl_->on_event;
  if (callback) callback({type, impl_->expected, operation, impl_->operations.source_size});
}
void Receiver::RegisterHost(const std::string& host_id, const std::string& generation,
                            const void* handle, CGRect anchor, CGRect passthrough,
                            NSArray<OCTPIPStackHostAnchor*>* anchors, OCTPIPStackScope scope,
                            OCTPIPStackHostRole role, bool home_available, bool animated) {
  MainThread();
  auto host_owner = impl_->host_owner.lock();
  Require(authenticated() && host_owner, "PiP native host registration requires its admitted receiver");
  host_owner->RegisterHost(host_id, generation, handle, anchor, passthrough, anchors,
                                 scope, role, home_available, animated);
}
id<OCTPIPStackAttachment> Receiver::AttachToParent(CALayer* parent, const std::string& id,
                                                 uint64_t generation, CGRect frame) {
  MainThread();
  Require(published() && Name(id) && Sequence(generation) && generation > impl_->last_generation,
          "PiP attachment requires a published current stream and a fresh generation");
  Require(parent && Frame(frame), "PiP stack did not supply its actual native item layer");
  auto attachment = std::make_shared<LayerAttachment>(parent, frame, impl_->operations.context_id);
  if (impl_->attachment) impl_->attachment->Detach();
  impl_->attachment.reset();
  impl_->attachment = std::move(attachment); impl_->attachment_id = id;
  impl_->generation = generation; impl_->last_generation = generation;
  return WrapAttachment(impl_->attachment);
}
void Receiver::Detach(const std::string& id, uint64_t generation) {
  MainThread();
  if (id != impl_->attachment_id || generation != impl_->generation) return;
  if (impl_->attachment) impl_->attachment->Detach();
  impl_->attachment.reset(); impl_->attachment_id.clear(); impl_->generation = 0;
}
void Receiver::RemoveHost(const std::string& host_id, const std::string& generation) {
  MainThread();
  if (auto host_owner = impl_->host_owner.lock()) host_owner->RemoveHost(host_id, generation);
}
void Receiver::Publish(NSXPCConnection* sender, const Identity& identity, uint32_t context, CGSize size) {
  CheckPeer(sender, identity);
  Require(!impl_->published && context && Positive(size), "PiP initial publication is invalid or duplicated");
  impl_->operations = {context, size, 0, 0}; impl_->published = true;
  NSError* error = nil;
  auto weak = weak_from_this();
  const Identity bound_identity = identity;
  const BOOL inserted = [impl_->stack publishRemoteItem:impl_->stack_identity sourceSize:size
      groupID:impl_->group_id batchID:impl_->batch_id factory:^id<OCTPIPStackAttachment>(CALayer* parent, CGSize display_size, NSError** failure) {
        try {
          auto receiver = weak.lock();
          Require(receiver != nullptr, "PiP native receiver was withdrawn");
          receiver->CheckPeer(sender, bound_identity);
          Require(receiver->impl_->last_generation < kMaxSequence, "PiP attachment generation exhausted");
          return receiver->AttachToParent(parent, bound_identity.presentation_id, receiver->impl_->last_generation + 1,
                                CGRectMake(0, 0, display_size.width, display_size.height));
        } catch (const std::exception& exception) {
          if (failure) *failure = [NSError errorWithDomain:@"OctopusNativePIP" code:1 userInfo:@{
            NSLocalizedDescriptionKey: [NSString stringWithUTF8String:exception.what()]}];
          return nil;
        }
      } animated:YES error:&error];
  if (!inserted) { Stop(); throw std::runtime_error("Native PiP stack could not publish the admitted source"); }
  Emit("published");
}
void Receiver::Prepare(NSXPCConnection* sender, const Identity& identity, uint64_t operation,
                       OperationKind kind, uint32_t context, CGSize source_size, SendRight fence) {
  CheckPeer(sender, identity);
  Require(impl_->published, "PiP preparation requires initial publication");
  impl_->operations.Validate(operation, kind, context, source_size);
  Require(MACH_PORT_VALID(fence.get()), "PiP preparation lacks a native fence");
  OperationState prepared = impl_->operations;
  prepared.Prepared(operation, kind, context, source_size);
  impl_->operations.pending = operation;
  const SendRight* borrowed = &fence;
  __block unsigned calls = 0;
  NSError* error = nil;
  const auto phase = kind == OperationKind::ResizeInProgress ? OCTPIPStackSourceProgress :
      kind == OperationKind::ReplaceContext ? OCTPIPStackSourceReplacement : OCTPIPStackSourceSettled;
  const BOOL applied = [impl_->stack applySourceSize:source_size identity:impl_->stack_identity phase:phase
      prepare:^BOOL(CGSize display_size, NSError** failure) {
        try {
          CheckPeer(sender, identity);
          Require(++calls == 1 && impl_->operations.pending == operation, "PiP source preparation was superseded or repeated");
          if (impl_->attachment && impl_->attachment->layer()) impl_->attachment->Prepare(kind, context, display_size, *borrowed);
          impl_->operations = prepared;
          return YES;
        } catch (const std::exception& exception) {
          if (failure) *failure = [NSError errorWithDomain:@"OctopusNativePIP" code:1 userInfo:@{
            NSLocalizedDescriptionKey: [NSString stringWithUTF8String:exception.what()]}];
          return NO;
        }
      } error:&error];
  if (!applied || calls != 1 || !authenticated() || impl_->operations.pending != operation) {
    Stop(); throw std::runtime_error("Native PiP preparation did not complete; presentation invalidated");
  }
  impl_->operations = prepared;
  Emit("prepared", operation);
}
void Receiver::Complete(NSXPCConnection* sender, const Identity& identity, uint64_t operation) {
  CheckPeer(sender, identity); impl_->operations.Complete(operation); Emit("committed", operation);
}
void Receiver::Invalidate(NSXPCConnection* sender, const Identity& identity) {
  CheckPeer(sender, identity); Stop();
}
void Receiver::ShowCompletion() {
  MainThread(); Require(published(), "Native PiP completion requires a published source");
  [impl_->stack showCompletionForItem:impl_->stack_identity applicationIcon:nil];
}
void Receiver::End(NSXPCConnection* sender, const Identity& identity, std::function<void()> completion) {
  CheckPeer(sender, identity); impl_->peer = nil; impl_->published = false;
  auto retained = shared_from_this();
  [impl_->stack removeItem:impl_->stack_identity animated:YES completion:^{
    retained->Stop(); if (completion) completion();
  }];
}
void Receiver::ConnectionLost(NSXPCConnection* sender) {
  MainThread();
  if (sender && sender == impl_->peer) { impl_->peer = nil; Stop(); }
}
void Receiver::Stop() {
  MainThread();
  if (impl_->stopped) return;
  impl_->stopped = true; impl_->peer = nil; impl_->published = false;
  [impl_->stack removeItem:impl_->stack_identity animated:NO completion:nil];
  if (impl_->attachment) impl_->attachment->Detach();
  impl_->attachment.reset();
  impl_->host_owner.reset(); impl_->operations.pending = 0;
  auto callback = std::move(impl_->on_event);
  if (callback) callback({"invalidated", impl_->expected, 0, impl_->operations.source_size});
}
}
