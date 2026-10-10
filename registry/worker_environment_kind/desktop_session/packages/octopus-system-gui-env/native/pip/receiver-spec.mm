#include "receiver.h"
#include "bootstrap.h"
#include <cassert>
#include <cmath>
#include <dlfcn.h>
#include <limits>
#include <stdexcept>
#include <utility>

@protocol SpecContext
+ (id)contextWithCGSConnection:(uint32_t)connection options:(NSDictionary*)options;
- (uint32_t)contextId;
- (mach_port_t)createFencePort;
- (void)setLayer:(CALayer*)layer;
@end

using namespace octopus::pip;
template<class F> void Reject(F call) {
  bool rejected = false;
  try { call(); } catch (const std::runtime_error&) { rejected = true; }
  assert(rejected);
}

int main(int argc, const char* argv[]) {
  @autoreleasepool {
    OperationState state{7, CGSizeMake(640, 360), 0, 0};
    Reject([&] { state.Validate(2, OperationKind::Resize, 7, CGSizeMake(800, 450)); });
    Reject([&] { state.Validate(1, OperationKind::Resize, 8, CGSizeMake(800, 450)); });
    Reject([&] { state.Validate(1, OperationKind::ReplaceContext, 7, CGSizeMake(800, 450)); });
    Reject([&] { state.Validate(1, OperationKind::Resize, 7, CGSizeMake(NAN, 450)); });
    state.Prepared(1, OperationKind::ResizeInProgress, 7, CGSizeMake(800, 450));
    assert(state.source_size.width == 640);
    Reject([&] { state.Validate(2, OperationKind::Resize, 7, CGSizeMake(800, 450)); });
    Reject([&] { state.Complete(2); });
    state.Complete(1);
    Reject([&] { state.Complete(1); });
    state.Prepared(2, OperationKind::ReplaceContext, 9, CGSizeMake(800, 450));
    state.Complete(2);
    assert(state.context_id == 9 && state.source_size.width == 800);
    state.completed = kMaxSequence;
    Reject([&] { state.Validate(kMaxSequence + 1, OperationKind::Resize, 9, CGSizeMake(800, 450)); });

    Identity expected{"host", "presentation", 1};
    Receiver receiver(expected, {});
    assert(!receiver.authenticated() && !receiver.published());
    Reject([&] { receiver.Publish(nil, expected, 7, CGSizeMake(640, 360)); });
    Reject([&] { receiver.Publish(nil, {"old-host", "presentation", 1}, 7, CGSizeMake(640, 360)); });
    Reject([&] { receiver.AttachToParent(nil, "attachment", 1, CGRectMake(0, 0, 640, 360)); });
    Reject([&] { receiver.RegisterHost("host", "generation", reinterpret_cast<void*>(1), CGRectZero,
                                     CGRectZero, @[], OCTPIPStackActive, OCTPIPStackMain, true, false); });
    receiver.Detach("stale", 999);
    receiver.Stop(); receiver.Stop();

    xpc_object_t invalid = xpc_dictionary_create(nullptr, nullptr, 0);
    xpc_dictionary_set_uint64(invalid, "fence", 123);
    Reject([&] { auto right = SendRight::CopyFence(invalid); });
    Class cls = NSClassFromString(@"CAContext");
    auto connection = reinterpret_cast<uint32_t (*)()>(dlsym(RTLD_DEFAULT, "CGSMainConnectionID"));
    auto send = reinterpret_cast<void (*)(xpc_object_t, const char*, mach_port_t)>(
        dlsym(RTLD_DEFAULT, "xpc_dictionary_set_mach_send"));
    assert(cls && connection && send && [cls respondsToSelector:@selector(contextWithCGSConnection:options:)]);
    id<SpecContext> producer = [(Class<SpecContext>)cls contextWithCGSConnection:connection() options:@{}];
    assert(producer && [(id)producer respondsToSelector:@selector(createFencePort)]);
    CALayer* source = [CALayer layer]; source.frame = CGRectMake(0, 0, 640, 360);
    [producer setLayer:source];
    mach_port_t port = [producer createFencePort]; assert(MACH_PORT_VALID(port));
    xpc_object_t payload = xpc_dictionary_create(nullptr, nullptr, 0);
    send(payload, "fence", port);
    mach_port_urefs_t before = 0, after = 0;
    assert(mach_port_get_refs(mach_task_self(), port, MACH_PORT_RIGHT_SEND, &before) == KERN_SUCCESS);
    CALayer* parent = [CALayer layer];
    {
      auto refused = SendRight::CopyFence(payload);
      Reject([&] { receiver.Prepare(nil, expected, 1, OperationKind::Resize, [producer contextId],
                                   CGSizeMake(640, 360), std::move(refused)); });
      assert(refused.get() == MACH_PORT_NULL);
    }
    assert(mach_port_get_refs(mach_task_self(), port, MACH_PORT_RIGHT_SEND, &after) == KERN_SUCCESS && after == before);
    {
      LayerAttachment attachment(parent, CGRectMake(10, 20, 640, 360), [producer contextId]);
      assert(parent.sublayers.count == 1 && attachment.layer().masksToBounds);
      assert(attachment.layer().sublayers.count == 1);
      auto copied = SendRight::CopyFence(payload);
      auto moved = std::move(copied); assert(copied.get() == MACH_PORT_NULL);
      attachment.Prepare(OperationKind::Resize, [producer contextId], CGSizeMake(800, 450), moved);
      attachment.SetFrame(CGRectMake(5, 10, 400, 225), true, true);
      assert(attachment.layer().frame.size.width == 400);
      attachment.Detach(); attachment.Detach();
      assert(parent.sublayers.count == 0);
    }
    assert(mach_port_get_refs(mach_task_self(), port, MACH_PORT_RIGHT_SEND, &after) == KERN_SUCCESS && after == before);
    assert(argc == 2);
    [NSApplication sharedApplication];
    auto first = [[OCTPIPStackIdentity alloc] initWithHostGeneration:@"host" presentationID:@"first" streamGeneration:1];
    auto second = [[OCTPIPStackIdentity alloc] initWithHostGeneration:@"host" presentationID:@"second" streamGeneration:1];
    NSMutableSet* current = [NSMutableSet setWithArray:@[first, second]];
    __block std::weak_ptr<StackHostOwner> native_owner;
    OCTPIPStackCallbacks* callbacks = [OCTPIPStackCallbacks new];
    callbacks.checkIdentity = ^BOOL(OCTPIPStackIdentity* identity) { return [current containsObject:identity]; };
    callbacks.checkHost = ^BOOL(NSString* id, NSString* generation, NSWindow* window) {
      auto owner = native_owner.lock(); return owner && owner->HasHost(id, generation, window);
    };
    callbacks.shouldShowGroup = ^BOOL(NSString*) { return YES; };
    callbacks.focusRequested = ^(OCTPIPStackIdentity*) {};
    callbacks.maxDisplaySizeRequested = ^(CGFloat) {};
    callbacks.hideGroupsRequested = ^(NSArray<NSString*>*, BOOL) {};
    callbacks.returnToMainRequested = ^BOOL(OCTPIPStackIdentity*) { return NO; };
    callbacks.sendToAccessoryRequested = ^BOOL(OCTPIPStackIdentity*) { return NO; };
    callbacks.hostChanged = ^(NSString*) {};
    callbacks.anchorCommitted = ^(NSString*, OCTPIPStackAlignment) {};
    callbacks.restSizeChanged = ^(CGSize) {};
    callbacks.lookTargetChanged = ^(CGPoint, BOOL) {};
    callbacks.interrupted = ^(OCTPIPStackIdentity*, NSError*) {};
    OCTPIPStackControlText* labels = [OCTPIPStackControlText new];
    labels.closeTooltip=@"Close"; labels.hide=@"Hide"; labels.hideForGroup=@"Hide source";
    labels.hideForAllGroups=@"Hide all"; labels.placementTooltip=@"Move";
    NSError* error = nil;
    OCTPIPStack* stack = [[OCTPIPStack alloc] initWithCallbacks:callbacks controlText:labels
        placementAssetURL:[NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]] error:&error];
    assert(stack && !error);
    auto host_owner = std::make_shared<StackHostOwner>(stack); native_owner = host_owner;
    NSWindow* window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,800,600)
        styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    host_owner->RegisterHost("client", "window-1", (__bridge const void*)window.contentView,
        CGRectMake(0,0,800,600), CGRectZero, @[], OCTPIPStackAll, OCTPIPStackMain, true, false);
    assert(host_owner->HasHost(@"client", @"window-1", window));
    __block std::shared_ptr<LayerAttachment> first_layer, second_layer;
    const uint32_t producer_id = [producer contextId];
    assert([stack publishRemoteItem:first sourceSize:CGSizeMake(640,360) groupID:@"group" batchID:@"batch"
        factory:^id<OCTPIPStackAttachment>(CALayer* parent, CGSize size, NSError**) {
          first_layer = std::make_shared<LayerAttachment>(parent, (CGRect){CGPointZero,size}, producer_id);
          return WrapAttachment(first_layer);
        } animated:NO error:&error]);
    assert([stack publishRemoteItem:second sourceSize:CGSizeMake(640,360) groupID:@"group" batchID:@"batch"
        factory:^id<OCTPIPStackAttachment>(CALayer* parent, CGSize size, NSError**) {
          second_layer = std::make_shared<LayerAttachment>(parent, (CGRect){CGPointZero,size}, producer_id);
          return WrapAttachment(second_layer);
        } animated:NO error:&error]);
    [current removeObject:first]; [stack removeItem:first animated:NO completion:nil];
    assert(!first_layer->layer() && second_layer->layer());
    assert(host_owner->HasHost(@"client", @"window-1", window));
    host_owner->RemoveHost("client", "obsolete-window");
    assert(host_owner->HasHost(@"client", @"window-1", window));
    [window close]; assert(!host_owner->HasHost(@"client", @"window-1", window));
    [stack close]; host_owner->Close(); assert(!second_layer->layer());
    auto images = ReceiverBootstrap::Create(labels,[NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]],[](NSDictionary*) {});
    NSImage* picture = [NSImage imageWithSize:NSMakeSize(32,16) flipped:NO drawingHandler:^BOOL(NSRect rect) {
      [NSColor.redColor setFill]; NSRectFill(rect); return YES;
    }];
    NSData* first_bytes = picture.TIFFRepresentation;
    NSDictionary* a = @{@"hostGeneration":@"browser-host",@"presentationId":@"browser:first",@"streamGeneration":@1};
    NSDictionary* b = @{@"hostGeneration":@"browser-host",@"presentationId":@"browser:second",@"streamGeneration":@2};
    images->UpsertBrowserImage(a,@"first-group",@"1",1,first_bytes);
    images->UpsertBrowserImage(a,@"first-group",@"1",1,first_bytes);
    images->UpsertBrowserImage(b,@"second-group",@"2",2,first_bytes);
    assert([images->Status()[@"browserImages"] unsignedIntegerValue] == 2);
    Reject([&] { images->UpsertBrowserImage(a,@"another-group",@"1",2,first_bytes); });
    images->UpsertBrowserImage(a,@"first-group",@"1",3,first_bytes);
    Reject([&] { images->UpsertBrowserImage(a,@"first-group",@"1",2,first_bytes); });
    NSImage* changed = [NSImage imageWithSize:NSMakeSize(32,16) flipped:NO drawingHandler:^BOOL(NSRect rect) {
      [NSColor.blueColor setFill]; NSRectFill(rect); return YES;
    }];
    Reject([&] { images->UpsertBrowserImage(a,@"first-group",@"1",3,changed.TIFFRepresentation); });
    images->CompleteGroup(@"first-group");
    assert([images->Status()[@"browserImages"] unsignedIntegerValue] == 1);
    images->InvalidateBrowserImage(a); images->InvalidateBrowserImage(b);
    assert([images->Status()[@"browserImages"] unsignedIntegerValue] == 0);
    images->Stop();
    Reject([&] { images->UpsertBrowserImage(a,@"first-group",@"1",4,first_bytes); });
    mach_port_deallocate(mach_task_self(), port);
    [producer setLayer:nil];
  }
}
