#ifndef OCTOPUS_NATIVE_PERMISSIONS_H
#define OCTOPUS_NATIVE_PERMISSIONS_H
#import <AppKit/AppKit.h>
#include <stdint.h>
#include <sys/types.h>
NS_ASSUME_NONNULL_BEGIN
@interface OCTAutomationPermission : NSObject
- (nullable instancetype)initWithRecipientProcessID:(pid_t)pid identifier:(NSString *)identifier error:(NSError *_Nullable *_Nullable)error;
- (NSDictionary *)recipientIdentity;
- (BOOL)isCurrent;
- (NSDictionary *)snapshot;
- (BOOL)begin:(NSString *)operation operationID:(NSString *)operationID timeoutMilliseconds:(uint64_t)timeout
    completion:(void (^)(NSDictionary *_Nullable,NSError *_Nullable))completion error:(NSError *_Nullable *_Nullable)error;
- (void)cancelSettings:(NSString *)operationID;
- (void)invalidate;
@end
@interface OCTPermissionWindowController : NSWindowController <NSWindowDelegate>
- (nullable instancetype)initWithOwner:(NSWindow *)owner
                           identifier:(NSString *)identifier
                           generation:(NSString *)generation
                             snapshot:(NSDictionary *)snapshot
                        resourceBundle:(NSBundle *)resourceBundle
                                event:(void (^)(NSDictionary *))event
                                error:(NSError *_Nullable *_Nullable)error;
- (BOOL)update:(NSDictionary *)snapshot sequence:(uint64_t)sequence
        errorText:(nullable NSString *)errorText error:(NSError *_Nullable *_Nullable)error;
- (BOOL)updateSettingsSource:(nullable NSDictionary *)source sequence:(uint64_t)sequence error:(NSError *_Nullable *_Nullable)error;
- (BOOL)hasGeneration:(NSString *)generation;
- (void)present;
- (void)invalidate;
@end
NS_ASSUME_NONNULL_END
#endif
