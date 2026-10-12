#ifndef OCTOPUS_NATIVE_PIP_STACK_H
#define OCTOPUS_NATIVE_PIP_STACK_H

#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#include <stdint.h>

NS_ASSUME_NONNULL_BEGIN

typedef NS_ENUM(NSInteger, OCTPIPStackAlignment) {
  OCTPIPStackTopLeft = 0, OCTPIPStackTopRight = 1,
  OCTPIPStackBottomRight = 2, OCTPIPStackBottomLeft = 3,
  OCTPIPStackTopCenter = 4, OCTPIPStackBottomCenter = 5
};
typedef NS_ENUM(NSInteger, OCTPIPStackScope) { OCTPIPStackActive, OCTPIPStackAll };
typedef NS_ENUM(NSInteger, OCTPIPStackHostRole) { OCTPIPStackMain, OCTPIPStackAccessory };
typedef NS_ENUM(NSInteger, OCTPIPStackSourcePhase) {
  OCTPIPStackSourceProgress, OCTPIPStackSourceSettled, OCTPIPStackSourceReplacement
};

@interface OCTPIPStackIdentity : NSObject <NSCopying>
@property(nonatomic, readonly, copy) NSString *hostGeneration;
@property(nonatomic, readonly, copy) NSString *presentationID;
@property(nonatomic, readonly) uint64_t streamGeneration;
- (instancetype)initWithHostGeneration:(NSString *)hostGeneration
                       presentationID:(NSString *)presentationID
                     streamGeneration:(uint64_t)streamGeneration;
@end

@protocol OCTPIPStackAttachment <NSObject>
- (BOOL)setFrameInCurrentTransaction:(CGRect)frame
             scaleFromCommittedSize:(BOOL)scale
                            visible:(BOOL)visible
                              error:(NSError *_Nullable *_Nullable)error;
- (void)detach;
@end

typedef id<OCTPIPStackAttachment> _Nullable (^OCTPIPStackAttachmentFactory)(
    CALayer *parent, CGSize displaySize, NSError *_Nullable *_Nullable error);
typedef BOOL (^OCTPIPStackSourcePrepare)(CGSize displaySize,
                                      NSError *_Nullable *_Nullable error);

@interface OCTPIPStackHostAnchor : NSObject
@property(nonatomic) CGPoint contentPoint;
@property(nonatomic) OCTPIPStackAlignment alignment;
- (instancetype)initWithPoint:(CGPoint)point alignment:(OCTPIPStackAlignment)alignment;
@end

@interface OCTPIPStackSpring : NSObject
@property(nonatomic) double mass;
@property(nonatomic) double stiffness;
@property(nonatomic) double damping;
@end

@interface OCTPIPStackCallbacks : NSObject
@property(nonatomic, copy) BOOL (^checkIdentity)(OCTPIPStackIdentity *identity);
@property(nonatomic, copy) BOOL (^checkHost)(NSString *hostID, NSString *generation, NSWindow *window);
@property(nonatomic, copy) BOOL (^shouldShowGroup)(NSString *groupID);
@property(nonatomic, copy) void (^focusRequested)(OCTPIPStackIdentity *identity);
@property(nonatomic, copy) void (^maxDisplaySizeRequested)(CGFloat size);
@property(nonatomic, copy) void (^hideGroupsRequested)(NSArray<NSString *> *groupIDs, BOOL all);
@property(nonatomic, copy) BOOL (^returnToMainRequested)(OCTPIPStackIdentity *identity);
@property(nonatomic, copy, nullable) BOOL (^sendToAccessoryRequested)(OCTPIPStackIdentity *identity);
@property(nonatomic, copy) void (^hostChanged)(NSString *_Nullable hostID);
@property(nonatomic, copy) void (^anchorCommitted)(NSString *hostID, OCTPIPStackAlignment alignment);
@property(nonatomic, copy) void (^restSizeChanged)(CGSize size);
@property(nonatomic, copy) void (^lookTargetChanged)(CGPoint cgPoint, BOOL available);
@property(nonatomic, copy) void (^interrupted)(OCTPIPStackIdentity *_Nullable identity, NSError *error);
@end

@interface OCTPIPStackControlText : NSObject
@property(nonatomic, copy) NSString *closeTooltip;
@property(nonatomic, copy) NSString *hide;
@property(nonatomic, copy) NSString *hideForGroup;
@property(nonatomic, copy) NSString *hideForAllGroups;
@property(nonatomic, copy) NSString *placementTooltip;
@end

@interface OCTPIPStack : NSObject
- (nullable instancetype)initWithCallbacks:(OCTPIPStackCallbacks *)callbacks
                              controlText:(OCTPIPStackControlText *)controlText
                        placementAssetURL:(NSURL *)assetURL
                                    error:(NSError *_Nullable *_Nullable)error;
- (BOOL)registerHost:(NSString *)hostID generation:(NSString *)generation
             window:(NSWindow *)window anchorContentRect:(CGRect)anchorRect
    passthroughRect:(CGRect)passthroughRect anchors:(NSArray<OCTPIPStackHostAnchor *> *)anchors
              scope:(OCTPIPStackScope)scope role:(OCTPIPStackHostRole)role
      homeAvailable:(BOOL)homeAvailable animated:(BOOL)animated
             spring:(nullable OCTPIPStackSpring *)spring
              error:(NSError *_Nullable *_Nullable)error;
- (void)removeHost:(NSString *)hostID generation:(NSString *)generation;
- (BOOL)embedItem:(OCTPIPStackIdentity *)identity viewID:(NSString *)viewID
      generation:(NSString *)generation hostID:(NSString *)hostID hostGeneration:(NSString *)hostGeneration
          window:(NSWindow *)window contentRect:(CGRect)rect error:(NSError *_Nullable *_Nullable)error;
- (void)detachEmbeddedView:(NSString *)viewID generation:(NSString *)generation;
- (void)hideEmbeddedView:(NSString *)viewID generation:(NSString *)generation;
- (BOOL)publishRemoteItem:(OCTPIPStackIdentity *)identity sourceSize:(CGSize)size
                 groupID:(NSString *)groupID batchID:(NSString *)batchID
                 factory:(OCTPIPStackAttachmentFactory)factory animated:(BOOL)animated
                   error:(NSError *_Nullable *_Nullable)error;
- (BOOL)publishImageItem:(OCTPIPStackIdentity *)identity image:(NSImage *)image
                groupID:(NSString *)groupID batchID:(NSString *)batchID animated:(BOOL)animated
                  error:(NSError *_Nullable *_Nullable)error;
- (BOOL)updateImage:(NSImage *)image identity:(OCTPIPStackIdentity *)identity
             error:(NSError *_Nullable *_Nullable)error;
- (BOOL)applySourceSize:(CGSize)size identity:(OCTPIPStackIdentity *)identity
                 phase:(OCTPIPStackSourcePhase)phase prepare:(OCTPIPStackSourcePrepare)prepare
                 error:(NSError *_Nullable *_Nullable)error;
- (void)removeItem:(OCTPIPStackIdentity *)identity animated:(BOOL)animated
       completion:(nullable void (^)(void))completion;
- (void)showCompletionForItem:(OCTPIPStackIdentity *)identity applicationIcon:(nullable NSImage *)icon;
- (BOOL)noteInteraction:(OCTPIPStackIdentity *)identity;
- (void)setActiveGroup:(nullable NSString *)groupID suppressedGroups:(NSSet<NSString *> *)suppressed;
- (void)refreshVisibilityAnimated:(BOOL)animated;
- (BOOL)setMaxDisplaySize:(CGFloat)size error:(NSError *_Nullable *_Nullable)error;
- (void)setPreferredAlignment:(OCTPIPStackAlignment)alignment floating:(BOOL)floating;
- (BOOL)moveToHost:(NSString *)hostID generation:(NSString *)generation
        alignment:(OCTPIPStackAlignment)alignment error:(NSError *_Nullable *_Nullable)error;
- (BOOL)moveFromScreenPoint:(CGPoint)start toScreenPoint:(CGPoint)end
                     error:(NSError *_Nullable *_Nullable)error;
- (void)invalidateIdentity:(OCTPIPStackIdentity *)identity error:(NSError *)error;
- (void)close;
@end

NS_ASSUME_NONNULL_END
#endif
