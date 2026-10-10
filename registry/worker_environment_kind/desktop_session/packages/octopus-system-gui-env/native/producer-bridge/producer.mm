#import "OctopusPIPBridge.h"
#import <AppKit/AppKit.h>
#import <CoreServices/CoreServices.h>
#import <objc/message.h>
#include <dlfcn.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <xpc/xpc.h>
#include <cmath>
#include <algorithm>
#include <climits>
#include <atomic>
#include <memory>

static NSError *Failure(NSString *message) {
    return [NSError errorWithDomain:@"octopus.desktop.pip" code:1 userInfo:@{NSLocalizedDescriptionKey:message}];
}
static BOOL Fail(NSError **error, NSString *message) { if (error) *error=Failure(message); return NO; }

BOOL OCTPIPConstantTimeEqual(NSData *left, NSData *right) {
    if (left.length != 32 || right.length != 32) return NO;
    const uint8_t *a=(const uint8_t *)left.bytes, *b=(const uint8_t *)right.bytes;
    volatile uint8_t difference=0;
    for (NSUInteger i=0;i<32;++i) difference |= a[i]^b[i];
    return difference==0;
}

@interface OCTPIPProducerFence ()
- (nullable instancetype)initWithPort:(mach_port_t)port error:(NSError **)error;
@property(nonatomic) mach_port_t port;
@property(nonatomic, strong) id payload;
@end
@implementation OCTPIPProducerFence
- (instancetype)initWithPort:(mach_port_t)port error:(NSError **)error {
    if (!(self=[super init])) return nil;
    _port=port;
    auto set=reinterpret_cast<void (*)(xpc_object_t,const char *,mach_port_t)>(dlsym(RTLD_DEFAULT,"xpc_dictionary_set_mach_send"));
    mach_port_type_t type=0;
    if (!MACH_PORT_VALID(port) || mach_port_type(mach_task_self(),port,&type)!=KERN_SUCCESS
            || !(type&MACH_PORT_TYPE_SEND) || !set) {
        Fail(error,@"Native producer fence does not carry an available Mach send right"); return nil;
    }
    xpc_object_t value=xpc_dictionary_create(nullptr,nullptr,0);
    set(value,"fence",port); _payload=value; return self;
}
- (void)dealloc { if (MACH_PORT_VALID(_port)) mach_port_deallocate(mach_task_self(),_port); }
@end

@interface OCTPIPProducerContext ()
- (nullable instancetype)initWithError:(NSError **)error;
@end
@implementation OCTPIPProducerContext {
    id _context;
    CALayer *_rootLayer;
    uint32_t _contextID;
}
+ (instancetype)makeWithError:(NSError **)error { return [[self alloc] initWithError:error]; }
- (instancetype)initWithError:(NSError **)error {
    if (!(self=[super init])) return nil;
    if (!NSThread.isMainThread) { Fail(error,@"Native producer context must be created on its owner thread"); return nil; }
    Class cls=NSClassFromString(@"CAContext");
    SEL make=NSSelectorFromString(@"contextWithCGSConnection:options:");
    auto main=reinterpret_cast<void *(*)()>(dlsym(RTLD_DEFAULT,"CGSMainConnectionID"));
    if (!cls || ![cls respondsToSelector:make] || !main) { Fail(error,@"Native remote layer context is unavailable"); return nil; }
    void *connection=main();
    if (!connection) { Fail(error,@"Native window server connection is unavailable"); return nil; }
    _context=reinterpret_cast<id (*)(id,SEL,void *,NSDictionary *)>(objc_msgSend)(cls,make,connection,@{});
    if (!_context || ![_context respondsToSelector:NSSelectorFromString(@"contextId")]
            || ![_context respondsToSelector:NSSelectorFromString(@"setLayer:")]
            || ![_context respondsToSelector:NSSelectorFromString(@"createFencePort")]
            || ![_context respondsToSelector:NSSelectorFromString(@"setFencePort:")]) {
        Fail(error,@"Native remote context does not expose its required layer and fence capabilities"); return nil;
    }
    _contextID=reinterpret_cast<uint32_t (*)(id,SEL)>(objc_msgSend)(_context,NSSelectorFromString(@"contextId"));
    if (!_contextID) { Fail(error,@"Native remote context has no identity"); return nil; }
    _rootLayer=[CALayer layer];
    reinterpret_cast<void (*)(id,SEL,id)>(objc_msgSend)(_context,NSSelectorFromString(@"setLayer:"),_rootLayer);
    return self;
}
- (CALayer *)rootLayer { return _rootLayer; }
- (uint32_t)contextID { return _contextID; }
- (OCTPIPProducerFence *)makeFenceWithError:(NSError **)error {
    if (!NSThread.isMainThread || !_context) { Fail(error,@"Native producer context is no longer live"); return nil; }
    mach_port_t port=reinterpret_cast<mach_port_t (*)(id,SEL)>(objc_msgSend)(_context,NSSelectorFromString(@"createFencePort"));
    return [[OCTPIPProducerFence alloc] initWithPort:port error:error];
}
- (BOOL)installFence:(OCTPIPProducerFence *)fence error:(NSError **)error {
    if (!NSThread.isMainThread || !_context || !MACH_PORT_VALID(fence.port)) return Fail(error,@"Native producer fence no longer belongs to a live context");
    reinterpret_cast<void (*)(id,SEL,mach_port_t)>(objc_msgSend)(_context,NSSelectorFromString(@"setFencePort:"),fence.port);
    return YES;
}
- (void)invalidate {
    if (_context) reinterpret_cast<void (*)(id,SEL,id)>(objc_msgSend)(_context,NSSelectorFromString(@"setLayer:"),nil);
    _context=nil; _rootLayer=nil; _contextID=0;
}
- (void)dealloc { [self invalidate]; }
@end

static NSXPCInterface *ReceiverInterface() {
    NSXPCInterface *value=[NSXPCInterface interfaceWithProtocol:@protocol(OCTPIPReceiver)];
    NSSet *identity=[NSSet setWithObjects:NSDictionary.class,NSString.class,NSNumber.class,nil];
    for (NSString *name in @[@"publishPresentation:contextID:width:height:reply:",
            @"preparePresentation:operationID:kind:contextID:width:height:fence:reply:",
            @"completePresentation:operationID:reply:",@"willEndPresentation:reply:",@"invalidatePresentation:reason:reply:",@"noteInteraction:reply:"]) {
        [value setClasses:identity forSelector:NSSelectorFromString(name) argumentIndex:0 ofReply:NO];
    }
    [value setXPCType:XPC_TYPE_DICTIONARY forSelector:@selector(preparePresentation:operationID:kind:contextID:width:height:fence:reply:) argumentIndex:6 ofReply:NO];
    return value;
}
static NSXPCInterface *PublisherInterface() {
    NSXPCInterface *value=[NSXPCInterface interfaceWithProtocol:@protocol(OCTPIPPublisher)];
    NSSet *identity=[NSSet setWithObjects:NSDictionary.class,NSString.class,NSNumber.class,nil];
    [value setClasses:identity forSelector:@selector(didEndPresentation:reply:) argumentIndex:0 ofReply:NO];
    [value setClasses:identity forSelector:@selector(requestFocus:reply:) argumentIndex:0 ofReply:NO];
    [value setClasses:identity forSelector:@selector(setMaxDisplaySize:identity:reply:) argumentIndex:1 ofReply:NO];
    return value;
}

@interface OCTPIPProducerTransport () <NSXPCListenerDelegate>
@end
@implementation OCTPIPProducerTransport {
    NSXPCListener *_listener;
    NSMutableSet<NSXPCConnection *> *_connections;
    NSMutableDictionary<NSNumber *,id> *_endpointTransfers;
    id<OCTPIPPublisher> (^_factory)(NSXPCConnection *);
    void (^_disconnected)(NSXPCConnection *);
    uint64_t (^_deadline)(pid_t);
    BOOL _closed;
}
- (instancetype)initWithFactory:(id<OCTPIPPublisher> (^)(NSXPCConnection *))factory
                   disconnected:(void (^)(NSXPCConnection *))disconnected
                 deadlineForPID:(uint64_t (^)(pid_t))deadline {
    if ((self=[super init])) {
        _factory=[factory copy]; _disconnected=[disconnected copy]; _deadline=[deadline copy];
        _connections=[NSMutableSet set]; _endpointTransfers=[NSMutableDictionary dictionary];
    }
    return self;
}
- (BOOL)startWithError:(NSError **)error {
    if (!NSThread.isMainThread || _closed || _listener) return Fail(error,@"Native publisher transport start is not an owner-thread transition");
    if (!dlsym(RTLD_DEFAULT,"xpc_pipe_create_from_port") || !dlsym(RTLD_DEFAULT,"xpc_pipe_routine")
            || !dlsym(RTLD_DEFAULT,"xpc_pipe_invalidate")) return Fail(error,@"Native endpoint transport is unavailable");
    _listener=[NSXPCListener anonymousListener]; _listener.delegate=self;
    if (![_listener.endpoint respondsToSelector:NSSelectorFromString(@"_endpoint")]) {
        [_listener invalidate]; _listener=nil; return Fail(error,@"Native listener endpoint transfer is unavailable");
    }
    [[NSAppleEventManager sharedAppleEventManager] setEventHandler:self andSelector:@selector(bootstrap:reply:)
        forEventClass:'OcPi' andEventID:'BtV1'];
    [_listener resume]; return YES;
}
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    __block BOOL accepted=NO;
    void (^accept)(void)=^{
        if (self->_closed || listener!=self->_listener || self->_connections.count>=32) return;
        id<OCTPIPPublisher> owner=self->_factory(connection);
        if (!owner) return;
        connection.exportedInterface=PublisherInterface(); connection.exportedObject=owner;
        connection.remoteObjectInterface=ReceiverInterface();
        __weak OCTPIPProducerTransport *weakSelf=self;
        __weak NSXPCConnection *weakConnection=connection;
        void (^lost)(void)=^{ dispatch_async(dispatch_get_main_queue(), ^{
            OCTPIPProducerTransport *strongSelf=weakSelf; NSXPCConnection *current=weakConnection;
            if (strongSelf && current && [strongSelf->_connections containsObject:current]) {
                [strongSelf->_connections removeObject:current]; strongSelf->_disconnected(current);
                current.exportedObject=nil; [current invalidate];
            }
        }); };
        connection.interruptionHandler=lost; connection.invalidationHandler=lost;
        [self->_connections addObject:connection]; [connection resume]; accepted=YES;
    };
    if (NSThread.isMainThread) accept(); else dispatch_sync(dispatch_get_main_queue(),accept);
    return accepted;
}
- (id<OCTPIPReceiver>)receiverForConnection:(NSXPCConnection *)connection expectedPID:(pid_t)pid error:(NSError **)error {
    if (!NSThread.isMainThread || _closed || ![_connections containsObject:connection]
            || pid<=0 || connection.processIdentifier!=pid) {
        Fail(error,@"Native receiver connection does not identify the current admitted process"); return nil;
    }
    __weak OCTPIPProducerTransport *weakSelf=self;
    __weak NSXPCConnection *weakConnection=connection;
    return [connection remoteObjectProxyWithErrorHandler:^(NSError *failure) {
        dispatch_async(dispatch_get_main_queue(), ^{
            OCTPIPProducerTransport *current=weakSelf; NSXPCConnection *peer=weakConnection;
            if (current && peer) [current closeConnection:peer];
        });
    }];
}
- (void)bootstrap:(NSAppleEventDescriptor *)event reply:(NSAppleEventDescriptor *)reply {
    if (_closed || !_listener || !NSThread.isMainThread) return;
    pid_t sender=0; Size length=0; DescType actual=0; mach_port_t port=MACH_PORT_NULL;
    OSStatus pidStatus=AEGetAttributePtr(event.aeDesc,keySenderPIDAttr,typeSInt32,&actual,&sender,sizeof(sender),&length);
    if (pidStatus!=noErr || actual!=typeSInt32 || length!=sizeof(sender) || sender<=0) return;
    uint64_t deadline=_deadline(sender);
    if (!deadline || mach_continuous_time()>=deadline || _endpointTransfers[@(sender)] || _endpointTransfers.count>=16) return;
    OSStatus portStatus=AEGetAttributePtr(event.aeDesc,'repp',typeMachPort,&actual,&port,sizeof(port),&length);
    mach_port_type_t portType=0;
    if (portStatus!=noErr || actual!=typeMachPort || length!=sizeof(port) || !MACH_PORT_VALID(port)
            || mach_port_type(mach_task_self(),port,&portType)!=KERN_SUCCESS || !(portType&MACH_PORT_TYPE_SEND)
            || mach_port_mod_refs(mach_task_self(),port,MACH_PORT_RIGHT_SEND,1)!=KERN_SUCCESS) return;
    auto create=reinterpret_cast<xpc_object_t (*)(mach_port_t,int)>(dlsym(RTLD_DEFAULT,"xpc_pipe_create_from_port"));
    auto routine=reinterpret_cast<int (*)(xpc_object_t,xpc_object_t,xpc_object_t *)>(dlsym(RTLD_DEFAULT,"xpc_pipe_routine"));
    auto invalidate=reinterpret_cast<void (*)(xpc_object_t)>(dlsym(RTLD_DEFAULT,"xpc_pipe_invalidate"));
    xpc_object_t endpoint=reinterpret_cast<xpc_object_t (*)(id,SEL)>(objc_msgSend)(_listener.endpoint,NSSelectorFromString(@"_endpoint"));
    xpc_object_t pipe=create(port,0); mach_port_deallocate(mach_task_self(),port);
    if (!endpoint || xpc_get_type(endpoint)!=XPC_TYPE_ENDPOINT || !pipe) return;
    _endpointTransfers[@(sender)]=pipe;
    xpc_object_t message=xpc_dictionary_create(nullptr,nullptr,0); xpc_dictionary_set_value(message,"endpoint",endpoint);
    mach_timebase_info_data_t scale; mach_timebase_info(&scale);
    uint64_t now=mach_continuous_time();
    long double remaining=deadline>now ? (long double)(deadline-now)*scale.numer/scale.denom : 0;
    int64_t nanos=(int64_t)std::min(remaining,(long double)INT64_MAX);
    auto completed=std::make_shared<std::atomic<bool>>(false);
    __weak OCTPIPProducerTransport *weakSelf=self;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,nanos),dispatch_get_main_queue(), ^{
        if (!completed->exchange(true)) {
            invalidate(pipe);
        }
    });
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0), ^{
        xpc_object_t response=nullptr;
        int status=routine(pipe,message,&response);
        (void)status; (void)response;
        invalidate(pipe);
        dispatch_async(dispatch_get_main_queue(), ^{
            completed->store(true);
            OCTPIPProducerTransport *owner=weakSelf;
            if (owner && owner->_endpointTransfers[@(sender)]==pipe) [owner->_endpointTransfers removeObjectForKey:@(sender)];
        });
    });
}
- (void)closeConnection:(NSXPCConnection *)connection {
    if ([_connections containsObject:connection]) {
        [_connections removeObject:connection]; _disconnected(connection);
        connection.exportedObject=nil; [connection invalidate];
    }
}
- (void)close {
    if (_closed) return; _closed=YES;
    [[NSAppleEventManager sharedAppleEventManager] removeEventHandlerForEventClass:'OcPi' andEventID:'BtV1'];
    [_listener invalidate]; _listener=nil;
    for (NSXPCConnection *connection in _connections.copy) [self closeConnection:connection];
    auto invalidate=reinterpret_cast<void (*)(xpc_object_t)>(dlsym(RTLD_DEFAULT,"xpc_pipe_invalidate"));
    if (invalidate) for (id pipe in _endpointTransfers.allValues) invalidate(pipe);
    [_endpointTransfers removeAllObjects];
}
- (void)dealloc { [self close]; }
@end
