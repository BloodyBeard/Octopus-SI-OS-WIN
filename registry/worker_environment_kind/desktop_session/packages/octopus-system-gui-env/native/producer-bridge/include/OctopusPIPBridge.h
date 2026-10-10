#ifndef OCTOPUS_PIP_PRODUCER_BRIDGE_H
#define OCTOPUS_PIP_PRODUCER_BRIDGE_H
#import <Foundation/Foundation.h>
#import <QuartzCore/QuartzCore.h>
#import "../../pip/transport.h"

NS_ASSUME_NONNULL_BEGIN
@interface OCTPIPProducerFence : NSObject
@property(nonatomic, readonly) id payload;
@end

@interface OCTPIPProducerContext : NSObject
- (instancetype)init NS_UNAVAILABLE;
+ (instancetype)new NS_UNAVAILABLE;
+ (nullable instancetype)makeWithError:(NSError **)error NS_SWIFT_NAME(make());
@property(nonatomic, readonly) CALayer *rootLayer;
@property(nonatomic, readonly) uint32_t contextID;
- (nullable OCTPIPProducerFence *)makeFenceWithError:(NSError **)error;
- (BOOL)installFence:(OCTPIPProducerFence *)fence error:(NSError **)error;
- (void)invalidate;
@end

@interface OCTPIPProducerTransport : NSObject
- (instancetype)initWithFactory:(id<OCTPIPPublisher> _Nullable (^)(NSXPCConnection *))factory
                   disconnected:(void (^)(NSXPCConnection *))disconnected
                 deadlineForPID:(uint64_t (^)(pid_t))deadline;
- (BOOL)startWithError:(NSError **)error;
- (nullable id<OCTPIPReceiver>)receiverForConnection:(NSXPCConnection *)connection
                                      expectedPID:(pid_t)pid error:(NSError **)error;
- (void)closeConnection:(NSXPCConnection *)connection;
- (void)close;
@end

FOUNDATION_EXPORT BOOL OCTPIPConstantTimeEqual(NSData *left, NSData *right);
NS_ASSUME_NONNULL_END
#endif
