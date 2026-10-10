#ifndef OCTOPUS_NATIVE_PIP_TRANSPORT_H
#define OCTOPUS_NATIVE_PIP_TRANSPORT_H
#import <Foundation/Foundation.h>
#include <stdint.h>

NS_ASSUME_NONNULL_BEGIN
@protocol OCTPIPPublisher <NSObject>
- (void)connectWithReply:(void (^)(NSError *_Nullable error))reply;
- (void)beginSourceAuthentication:(NSData *)bindingBytes
               receiverChallenge:(NSData *)receiverChallenge
                           reply:(void (^)(NSData *_Nullable producerChallenge,
                                           NSData *_Nullable producerProof,
                                           NSError *_Nullable error))reply;
- (void)finishSourceAuthentication:(NSString *)bootstrapID
                    receiverProof:(NSData *)receiverProof
                            reply:(void (^)(NSError *_Nullable error))reply;
- (void)activateSource:(NSString *)bootstrapID reply:(void (^)(NSError *_Nullable error))reply;
- (void)cancelSource:(NSString *)bootstrapID reply:(void (^)(NSError *_Nullable error))reply;
- (void)didEndPresentation:(NSDictionary *)identity reply:(void (^)(NSError *_Nullable error))reply;
- (void)requestFocus:(NSDictionary *)identity reply:(void (^)(NSError *_Nullable error))reply;
- (void)setMaxDisplaySize:(double)size identity:(NSDictionary *)identity
                   reply:(void (^)(NSError *_Nullable error))reply;
@end

@protocol OCTPIPReceiver <NSObject>
- (void)publishPresentation:(NSDictionary *)identity contextID:(uint32_t)contextID
                     width:(double)width height:(double)height
                     reply:(void (^)(NSError *_Nullable error))reply;
- (void)preparePresentation:(NSDictionary *)identity operationID:(uint64_t)operationID
                      kind:(NSString *)kind contextID:(uint32_t)contextID
                     width:(double)width height:(double)height fence:(id)fence
                     reply:(void (^)(NSError *_Nullable error))reply;
- (void)completePresentation:(NSDictionary *)identity operationID:(uint64_t)operationID
                      reply:(void (^)(NSError *_Nullable error))reply;
- (void)willEndPresentation:(NSDictionary *)identity reply:(void (^)(NSError *_Nullable error))reply;
- (void)invalidatePresentation:(NSDictionary *)identity reason:(NSString *)reason
                        reply:(void (^)(NSError *_Nullable error))reply;
- (void)noteInteraction:(NSDictionary *)identity reply:(void (^)(NSError *_Nullable error))reply;
@end
NS_ASSUME_NONNULL_END
#endif
