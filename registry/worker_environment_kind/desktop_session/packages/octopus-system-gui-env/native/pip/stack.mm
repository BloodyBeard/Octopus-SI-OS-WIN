#import "stack.h"
#import <CoreImage/CoreImage.h>
#import <CoreVideo/CoreVideo.h>
#import <CommonCrypto/CommonDigest.h>
#import <objc/message.h>
#include <algorithm>
#include <atomic>
#include <cmath>
#include <memory>
#include <dlfcn.h>

static NSError *PIPError(NSString *message) {
  return [NSError errorWithDomain:@"Octopus.PIP.Stack" code:1 userInfo:@{NSLocalizedDescriptionKey:message}];
}
static BOOL PIPFail(NSError **error, NSString *message) { if (error) *error=PIPError(message); return NO; }
static void PIPMain(void) { if(!NSThread.isMainThread) @throw [NSException exceptionWithName:NSInternalInconsistencyException reason:@"PiP presentation requires the main thread." userInfo:nil]; }
static BOOL PIPSize(CGSize s) { return std::isfinite(s.width)&&std::isfinite(s.height)&&s.width>0&&s.height>0; }
static BOOL PIPPoint(CGPoint p) { return std::isfinite(p.x)&&std::isfinite(p.y); }
static BOOL PIPRect(CGRect r) { return PIPPoint(r.origin)&&PIPSize(r.size); }
static CGPoint PIPAdd(CGPoint a, CGPoint b) { return CGPointMake(a.x+b.x,a.y+b.y); }
static CGPoint PIPSub(CGPoint a, CGPoint b) { return CGPointMake(a.x-b.x,a.y-b.y); }
static CGPoint PIPMul(CGPoint a, double k) { return CGPointMake(a.x*k,a.y*k); }
static double PIPLength(CGPoint a) { return std::hypot(a.x,a.y); }
static double PIPAxis(OCTPIPStackAlignment a) { return a>=4?.5:(a==0||a==3?0:1); }
static BOOL PIPUp(OCTPIPStackAlignment a) { return a==2||a==3||a==5; }
static CGPoint PIPOffset(CGSize s, NSUInteger i, OCTPIPStackAlignment a) {
  double x=PIPAxis(a); return CGPointMake(-s.width*x+(1-2*x)*i*28,PIPUp(a)?i*22:-s.height-i*22);
}
static CGPoint PIPFixed(CGSize s, OCTPIPStackAlignment a) { return CGPointMake(s.width*PIPAxis(a),PIPUp(a)?0:s.height); }
static CGSize PIPFit(CGSize source, CGFloat limit) {
  double scale=std::min(limit/source.width,limit/source.height);
  return CGSizeMake(std::max(1.,source.width*scale),std::max(1.,source.height*scale));
}
static NSValue *PIPValue(CGPoint p) { return [NSValue valueWithPoint:p]; }
static CGColorRef PIPColor(CGFloat r, CGFloat g, CGFloat b, CGFloat a) {
  return [NSColor colorWithSRGBRed:r green:g blue:b alpha:a].CGColor;
}
static void PIPRoundPath(CALayer *layer, CGSize size, CGFloat radius) {
  CGPathRef p=radius>0?CGPathCreateWithRoundedRect((CGRect){CGPointZero,size},radius,radius,NULL)
                     :CGPathCreateWithRect((CGRect){CGPointZero,size},NULL);
  layer.shadowPath=p; CGPathRelease(p);
}
static id PIPFilter(NSString *name) {
  Class c=NSClassFromString(@"CAFilter"); SEL s=NSSelectorFromString(@"filterWithName:");
  if (!c||![c respondsToSelector:s]) @throw [NSException exceptionWithName:@"PIPUnavailable" reason:@"Native layer filter is unavailable." userInfo:nil];
  id f=((id(*)(id,SEL,id))objc_msgSend)(c,s,name);
  if (!f) @throw [NSException exceptionWithName:@"PIPUnavailable" reason:[@"Native layer filter is unavailable: " stringByAppendingString:name] userInfo:nil];
  return f;
}
static CALayer *PIPBackdrop(void) {
  Class c=NSClassFromString(@"CABackdropLayer");
  CALayer *l=c?[[c alloc] init]:nil;
  NSArray<NSString*> *selectors=@[@"setGroupName:",@"setScale:",@"setWindowServerAware:",@"setAllowsInPlaceFiltering:",@"setCaptureOnly:"];
  for (NSString *s in selectors) if (!l||![l respondsToSelector:NSSelectorFromString(s)])
    @throw [NSException exceptionWithName:@"PIPUnavailable" reason:@"Native backdrop layer is unavailable." userInfo:nil];
  ((void(*)(id,SEL,id))objc_msgSend)(l,NSSelectorFromString(selectors[0]),NSUUID.UUID.UUIDString);
  ((void(*)(id,SEL,CGFloat))objc_msgSend)(l,NSSelectorFromString(selectors[1]),1);
  ((void(*)(id,SEL,BOOL))objc_msgSend)(l,NSSelectorFromString(selectors[2]),YES);
  ((void(*)(id,SEL,BOOL))objc_msgSend)(l,NSSelectorFromString(selectors[3]),NO);
  ((void(*)(id,SEL,BOOL))objc_msgSend)(l,NSSelectorFromString(selectors[4]),NO);
  return l;
}
static CABasicAnimation *PIPBasic(NSString *key,id from,id to,double duration,NSString *timing) {
  CABasicAnimation *a=[CABasicAnimation animationWithKeyPath:key];
  a.fromValue=from; a.toValue=to; a.duration=duration; a.timingFunction=[CAMediaTimingFunction functionWithName:timing];
  return a;
}
static CASpringAnimation *PIPSpring(NSString *key,id from,id to,double stiffness,double damping) {
  CASpringAnimation *a=[CASpringAnimation animationWithKeyPath:key];
  a.mass=1; a.stiffness=stiffness; a.damping=damping; a.initialVelocity=0;
  a.fromValue=from; a.toValue=to; a.duration=a.settlingDuration; return a;
}
static CGImageRef PIPCGImage(NSImage *image) { return [image CGImageForProposedRect:NULL context:nil hints:nil]; }
static NSImage *PIPControlImage(NSImage *source,CGFloat extent,CGFloat yOffset,CGFloat backing) {
  if(!source||!PIPSize(source.size)||backing<=0)return nil;
  NSInteger pixels=(NSInteger)std::ceil(22*backing);
  NSBitmapImageRep *rep=[[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL pixelsWide:pixels pixelsHigh:pixels bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO colorSpaceName:NSDeviceRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
  if(!rep)return nil;rep.size=NSMakeSize(22,22);
  [NSGraphicsContext saveGraphicsState];NSGraphicsContext.currentContext=[NSGraphicsContext graphicsContextWithBitmapImageRep:rep];
  CGSize size=PIPFit(source.size,extent);CGRect rect=CGRectMake((22-size.width)/2,(22-size.height)/2+yOffset,size.width,size.height);
  [source drawInRect:rect fromRect:NSZeroRect operation:NSCompositingOperationSourceOver fraction:1 respectFlipped:NO hints:nil];
  [NSColor.whiteColor setFill];NSRectFillUsingOperation(CGRectMake(0,0,22,22),NSCompositingOperationSourceIn);[NSGraphicsContext restoreGraphicsState];
  NSImage *image=[[NSImage alloc] initWithSize:NSMakeSize(22,22)];[image addRepresentation:rep];return image;
}
static CIFilter *PIPControlColor(NSString *name,double white) {
  CIFilter *filter=[CIFilter filterWithName:@"CIColorMatrix"];filter.name=name;
  for(NSString *key in @[@"inputRVector",@"inputGVector",@"inputBVector"])[filter setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:0] forKey:key];
  [filter setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:1] forKey:@"inputAVector"];
  [filter setValue:[CIVector vectorWithX:white Y:white Z:white W:0] forKey:@"inputBiasVector"];return filter;
}

@implementation OCTPIPStackIdentity
- (instancetype)initWithHostGeneration:(NSString *)h presentationID:(NSString *)p streamGeneration:(uint64_t)g {
  if (!(self=[super init])) return nil;
  if (!h.length||!p.length||!g) return nil;
  _hostGeneration=[h copy]; _presentationID=[p copy]; _streamGeneration=g; return self;
}
- (id)copyWithZone:(NSZone *)zone { return self; }
- (NSUInteger)hash { return _hostGeneration.hash^_presentationID.hash^(NSUInteger)_streamGeneration; }
- (BOOL)isEqual:(id)o {
  return [o isKindOfClass:OCTPIPStackIdentity.class]&&_streamGeneration==[o streamGeneration]
    &&[_hostGeneration isEqual:[o hostGeneration]]&&[_presentationID isEqual:[o presentationID]];
}
@end
@implementation OCTPIPStackHostAnchor
- (instancetype)initWithPoint:(CGPoint)p alignment:(OCTPIPStackAlignment)a { if ((self=[super init])) {_contentPoint=p;_alignment=a;} return self; }
@end
@implementation OCTPIPStackSpring
@end
@implementation OCTPIPStackCallbacks
@end
@implementation OCTPIPStackControlText
@end

@interface PIPHost : NSObject
@property(nonatomic,copy) NSString *identifier;
@property(nonatomic,copy) NSString *generation;
@property(nonatomic,weak) NSWindow *window;
@property(nonatomic) CGRect contentRect;
@property(nonatomic) CGRect passthrough;
@property(nonatomic,copy) NSArray<OCTPIPStackHostAnchor *> *anchors;
@property(nonatomic) OCTPIPStackScope scope;
@property(nonatomic) OCTPIPStackHostRole role;
@property(nonatomic) BOOL homeAvailable;
- (CGRect)screenRect;
- (CGRect)passthroughScreenRect;
- (CGPoint)screenPoint:(CGPoint)point;
@end
@implementation PIPHost
- (CGRect)screenRect { NSWindow *w=_window; return w?[w convertRectToScreen:[w.contentView convertRect:_contentRect toView:nil]]:CGRectZero; }
- (CGRect)passthroughScreenRect { NSWindow *w=_window; return w&&!CGRectIsEmpty(_passthrough)?[w convertRectToScreen:[w.contentView convertRect:_passthrough toView:nil]]:CGRectZero; }
- (CGPoint)screenPoint:(CGPoint)p { NSWindow *w=_window; return [w convertPointToScreen:[w.contentView convertPoint:p toView:nil]]; }
@end

@interface PIPEmbeddedView : NSView
@end
@implementation PIPEmbeddedView
- (NSView *)hitTest:(NSPoint)point { return nil; }
- (BOOL)acceptsFirstResponder { return NO; }
@end

@interface PIPAnchor : NSObject
@property(nonatomic) CGPoint point;
@property(nonatomic) OCTPIPStackAlignment alignment;
@property(nonatomic,strong) PIPHost *host;
+ (instancetype)at:(CGPoint)p alignment:(OCTPIPStackAlignment)a host:(PIPHost *)host;
@end
@implementation PIPAnchor
+ (instancetype)at:(CGPoint)p alignment:(OCTPIPStackAlignment)a host:(PIPHost *)h {
  PIPAnchor *v=[self new];v.point=p;v.alignment=a;v.host=h;return v;
}
@end

@interface PIPItem : NSObject
@property(nonatomic,strong) PIPEmbeddedView *embeddedView;
@property(nonatomic,copy) NSString *embeddedID;
@property(nonatomic,copy) NSString *embeddedGeneration;
@property(nonatomic,strong) PIPHost *embeddedHost;
@property(nonatomic) BOOL embeddedVisible;
@property(nonatomic,strong) OCTPIPStackIdentity *identity;
@property(nonatomic,copy) NSString *groupID;
@property(nonatomic,copy) NSString *batchID;
@property(nonatomic,strong) CALayer *root;
@property(nonatomic,strong) CALayer *appearance;
@property(nonatomic,strong) CALayer *shadow;
@property(nonatomic,strong) CALayer *content;
@property(nonatomic,strong) CALayer *imageLayer;
@property(nonatomic,strong) CIFilter *appearanceBlur;
@property(nonatomic,strong) id<OCTPIPStackAttachment> attachment;
@property(nonatomic,copy) OCTPIPStackAttachmentFactory factory;
@property(nonatomic,strong) NSImage *image;
@property(nonatomic,strong) NSImage *completionIcon;
@property(nonatomic) CGSize sourceSize;
@property(nonatomic) CGSize displaySize;
@property(nonatomic) CGFloat maxSize;
@property(nonatomic) BOOL imageItem;
@property(nonatomic) BOOL visible;
@property(nonatomic) BOOL contributes;
@property(nonatomic) BOOL detached;
@property(nonatomic) BOOL scalesContent;
@property(nonatomic) BOOL completed;
@property(nonatomic) BOOL hovered;
@property(nonatomic) CGRect frame;
@property(nonatomic) CGPoint origin;
@property(nonatomic) CGPoint target;
@property(nonatomic) CGPoint velocity;
@property(nonatomic) CGPoint restOffset;
@property(nonatomic) double stiffness;
@property(nonatomic) double damping;
@property(nonatomic) BOOL initialized;
@property(nonatomic) BOOL leads;
@property(nonatomic) uint64_t appearanceGeneration;
@property(nonatomic) uint64_t effectGeneration;
@property(nonatomic,strong) CALayer *shade;
@property(nonatomic,strong) CALayer *iconLayer;
@property(nonatomic,strong) CALayer *iconMask;
@property(nonatomic,strong) CALayer *checkLayer;
@property(nonatomic,strong) CALayer *badge;
@property(nonatomic,strong) CALayer *hoverLayer;
@property(nonatomic,copy) void (^removalCompletion)(void);
@property(nonatomic,copy) BOOL (^valid)(void);
- (instancetype)initWithSize:(CGSize)size max:(CGFloat)max image:(nullable NSImage *)image;
- (BOOL)layout:(CGRect)frame error:(NSError **)error;
- (void)appearanceVisible:(BOOL)visible animated:(BOOL)animated completion:(nullable void (^)(void))completion;
- (void)setHover:(BOOL)hover;
- (void)showCompletion:(nullable NSImage *)icon;
- (void)stopCompletion;
- (void)layoutCompletion;
- (void)detach;
@end

@implementation PIPItem
- (instancetype)initWithSize:(CGSize)s max:(CGFloat)m image:(NSImage *)image {
  if (!(self=[super init])) return nil;
  _sourceSize=s;_maxSize=m;_displaySize=PIPFit(s,m);_image=image;_imageItem=image!=nil;
  _root=[CALayer layer];_appearance=[CALayer layer];_shadow=[CALayer layer];_content=[CALayer layer];
  [_root addSublayer:_appearance];[_appearance addSublayer:_shadow];[_shadow addSublayer:_content];
  _shadow.shadowColor=NSColor.blackColor.CGColor;_shadow.shadowOpacity=.16;
  _shadow.shadowOffset=CGSizeMake(0,-6);_shadow.shadowRadius=10;
  _appearance.opacity=0;_appearanceBlur=[CIFilter filterWithName:@"CIGaussianBlur"];
  if (!_appearanceBlur) return nil;
  _appearanceBlur.name=@"remoteHostedPIPAppearanceBlur";
  if (_imageItem) {
    _imageLayer=[CALayer layer];_imageLayer.cornerRadius=8;_imageLayer.cornerCurve=kCACornerCurveContinuous;
    _imageLayer.masksToBounds=YES;_imageLayer.contentsGravity=kCAGravityResizeAspectFill;
    _imageLayer.contents=(__bridge id)PIPCGImage(image);[_content addSublayer:_imageLayer];
  }
  return self;
}
- (BOOL)layout:(CGRect)f error:(NSError **)error {
  if (_detached) return PIPFail(error,@"PiP item is detached.");
  _frame=f;_root.frame=f;
  CGRect local=(CGRect){CGPointZero,f.size};
  _appearance.frame=local;_shadow.frame=local;_content.frame=local;_imageLayer.frame=local;
  _hoverLayer.frame=local;
  PIPRoundPath(_shadow,f.size,_imageItem?8:0);
  if (_attachment&&![_attachment setFrameInCurrentTransaction:local scaleFromCommittedSize:_scalesContent visible:_contributes error:error]) return NO;
  [self layoutCompletion];return YES;
}
- (void)appearanceVisible:(BOOL)show animated:(BOOL)animated completion:(void (^)(void))completion {
  uint64_t generation=++_appearanceGeneration;_visible=show;
  if (show) {_contributes=YES;_root.hidden=NO;}
  [_appearance removeAllAnimations];
  if (!animated) {
    [CATransaction begin];[CATransaction setDisableActions:YES];
    _appearance.transform=CATransform3DIdentity;_appearance.opacity=show?1:0;_appearance.filters=nil;
    _root.hidden=!show;_contributes=show;[CATransaction commit];
    if(completion)completion();return;
  }
  double from=show?.72:1,to=show?1:.72;
  CASpringAnimation *scale=PIPSpring(@"transform.scale",@(from),@(to),185,18);
  CABasicAnimation *opacity=PIPBasic(@"opacity",@(show?0:1),@(show?1:0),show?.20:.22,show?kCAMediaTimingFunctionEaseOut:kCAMediaTimingFunctionEaseIn);
  CABasicAnimation *blur=PIPBasic(@"filters.remoteHostedPIPAppearanceBlur.inputRadius",@(show?18:0),@(show?0:18),show?.28:.24,show?kCAMediaTimingFunctionEaseOut:kCAMediaTimingFunctionEaseIn);
  CAAnimationGroup *group=[CAAnimationGroup animation];group.animations=@[scale,opacity,blur];
  group.duration=std::max(scale.settlingDuration,blur.duration);group.removedOnCompletion=YES;
  [CATransaction begin];[CATransaction setDisableActions:YES];
  [_appearanceBlur setValue:@(show?0:18) forKey:kCIInputRadiusKey];_appearance.filters=@[_appearanceBlur];
  _appearance.transform=CATransform3DMakeScale(to,to,1);_appearance.opacity=show?1:0;
  [_appearance addAnimation:group forKey:@"appearance"];[CATransaction commit];
  __weak PIPItem *weak=self;
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW,(int64_t)(group.duration*NSEC_PER_SEC)),dispatch_get_main_queue(),^{
    PIPItem *item=weak;
    if(!item||item.detached||item.appearanceGeneration!=generation) return;
    if(show&&item.valid&&!item.valid()) return;
    [CATransaction begin];[CATransaction setDisableActions:YES];
    item.appearance.filters=nil;item.root.hidden=!show;item.contributes=show;
    [CATransaction commit];if(completion)completion();
  });
}
- (void)setHover:(BOOL)hover {
  if (_hovered==hover) return;_hovered=hover;
  if(!_hoverLayer&&hover) {_hoverLayer=[CALayer layer];_hoverLayer.compositingFilter=@"sourceAtop";[_content addSublayer:_hoverLayer];}
  _hoverLayer.frame=_content.bounds;_hoverLayer.backgroundColor=PIPColor(0,0,0,.06);_hoverLayer.hidden=!hover;
}
- (void)layoutCompletion {
  CGSize s=_content.bounds.size;CGPoint center=CGPointMake(s.width/2,s.height/2);
  _shade.frame=_content.bounds;_iconLayer.bounds=CGRectMake(0,0,50,50);_iconLayer.position=center;
  _iconMask.frame=_iconLayer.bounds;
  if(_checkLayer&&![_checkLayer valueForKey:@"mini"])_checkLayer.position=center;
  if([[_checkLayer valueForKey:@"mini"] boolValue]) {_checkLayer.position=PIPAdd(center,CGPointMake(18,-18));_badge.position=_checkLayer.position;}
}
- (void)fullCheck:(uint64_t)generation {
  if(_detached||generation!=_effectGeneration||(_valid&&!_valid()))return;
  [_iconMask addAnimation:PIPBasic(@"opacity",@0,@.65,.25,kCAMediaTimingFunctionEaseOut) forKey:@"dim"];
  _iconMask.opacity=.65;_checkLayer.opacity=1;_checkLayer.transform=CATransform3DIdentity;
  [_checkLayer addAnimation:PIPSpring(@"transform.scale",@.01,@1,170,18) forKey:@"scale"];
  [_checkLayer addAnimation:PIPBasic(@"opacity",@0,@1,.20,kCAMediaTimingFunctionEaseOut) forKey:@"opacity"];
}
- (void)miniCheck:(uint64_t)generation {
  if(_detached||generation!=_effectGeneration||(_valid&&!_valid()))return;
  CGPoint center=CGPointMake(_content.bounds.size.width/2,_content.bounds.size.height/2),target=PIPAdd(center,CGPointMake(18,-18));
  [_checkLayer setValue:@YES forKey:@"mini"];
  _checkLayer.position=target;_checkLayer.transform=CATransform3DMakeScale(.5,.5,1);
  CIFilter *black=[CIFilter filterWithName:@"CIColorMatrix"];
  [black setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:0] forKey:@"inputRVector"];
  [black setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:0] forKey:@"inputGVector"];
  [black setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:0] forKey:@"inputBVector"];
  [black setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:.5] forKey:@"inputAVector"];
  _checkLayer.filters=@[black];
  [_checkLayer addAnimation:PIPSpring(@"position",PIPValue(center),PIPValue(target),170,18) forKey:@"position"];
  [_checkLayer addAnimation:PIPSpring(@"transform.scale",@1,@.5,170,18) forKey:@"scale"];
  _badge=[CALayer layer];_badge.bounds=CGRectMake(0,0,17.5,17.5);_badge.position=target;_badge.cornerRadius=8.75;
  _badge.backgroundColor=PIPColor(0,230./255,45./255,1);_badge.shadowColor=NSColor.blackColor.CGColor;
  _badge.shadowOpacity=.8;_badge.shadowRadius=4;_badge.shadowOffset=CGSizeZero;
  [_content insertSublayer:_badge below:_checkLayer];
  [_badge addAnimation:PIPSpring(@"position",PIPValue(center),PIPValue(target),170,18) forKey:@"position"];
  [_badge addAnimation:PIPSpring(@"transform.scale",@.01,@1,170,18) forKey:@"scale"];
  [_badge addAnimation:PIPBasic(@"opacity",@0,@1,.20,kCAMediaTimingFunctionEaseOut) forKey:@"opacity"];
  [_iconMask addAnimation:PIPBasic(@"opacity",@.65,@0,.25,kCAMediaTimingFunctionEaseOut) forKey:@"dim"];
  _iconMask.opacity=0;
}
- (void)showCompletion:(NSImage *)icon {
  [self stopCompletion];_completed=YES;_completionIcon=icon;uint64_t generation=_effectGeneration;
  _shade=[CALayer layer];_shade.backgroundColor=PIPColor(0,0,0,.17);_shade.compositingFilter=@"sourceAtop";
  [_content addSublayer:_shade];
  [_shade addAnimation:PIPBasic(@"opacity",@0,@1,.35,kCAMediaTimingFunctionEaseOut) forKey:@"opacity"];
  NSImage *check=[NSImage imageWithSystemSymbolName:@"checkmark" accessibilityDescription:nil];
  check=[check imageWithSymbolConfiguration:[NSImageSymbolConfiguration configurationWithPointSize:18 weight:NSFontWeightBold]];
  _checkLayer=[CALayer layer];_checkLayer.bounds=CGRectMake(0,0,18,18);_checkLayer.contents=(__bridge id)PIPCGImage(check);
  _checkLayer.contentsGravity=kCAGravityResizeAspect;_checkLayer.opacity=0;
  CIFilter *white=[CIFilter filterWithName:@"CIColorMatrix"];
  for(NSString *key in @[@"inputRVector",@"inputGVector",@"inputBVector"]) [white setValue:[CIVector vectorWithX:0 Y:0 Z:0 W:0] forKey:key];
  [white setValue:[CIVector vectorWithX:1 Y:1 Z:1 W:0] forKey:@"inputBiasVector"];_checkLayer.filters=@[white];
  if(icon&&PIPCGImage(icon)) {
    _iconLayer=[CALayer layer];_iconLayer.contents=(__bridge id)PIPCGImage(icon);_iconLayer.contentsGravity=kCAGravityResizeAspect;
    [_content addSublayer:_iconLayer];[_iconLayer addAnimation:PIPBasic(@"opacity",@0,@1,.35,kCAMediaTimingFunctionEaseOut) forKey:@"opacity"];
    _iconMask=[CALayer layer];_iconMask.backgroundColor=NSColor.blackColor.CGColor;_iconMask.opacity=0;_iconMask.compositingFilter=@"sourceAtop";
    [_iconLayer addSublayer:_iconMask];[_content addSublayer:_checkLayer];
    __weak PIPItem *weak=self;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,NSEC_PER_SEC),dispatch_get_main_queue(),^{
      PIPItem *item=weak;if(!item||item.detached||item.effectGeneration!=generation)return;[item fullCheck:generation];
      dispatch_after(dispatch_time(DISPATCH_TIME_NOW,1500*NSEC_PER_MSEC),dispatch_get_main_queue(),^{[weak miniCheck:generation];});
    });
  } else {[_content addSublayer:_checkLayer];[self fullCheck:generation];}
  [self layoutCompletion];
}
- (void)stopCompletion {
  ++_effectGeneration;_completed=NO;
  for(id l in @[_shade?:[NSNull null],_iconLayer?:[NSNull null],_iconMask?:[NSNull null],_checkLayer?:[NSNull null],_badge?:[NSNull null]])
    if([l isKindOfClass:CALayer.class]){[l removeAllAnimations];[l removeFromSuperlayer];}
  _shade=nil;_iconLayer=nil;_iconMask=nil;_checkLayer=nil;_badge=nil;_completionIcon=nil;
}
- (void)detach {
  if(_detached)return;_detached=YES;++_appearanceGeneration;[self stopCompletion];
  [_attachment detach];_attachment=nil;_root.hidden=YES;[_root removeAllAnimations];[_appearance removeAllAnimations];
  [_root removeFromSuperlayer];_contributes=NO;_visible=NO;_valid=nil;
}
@end

@interface PIPDrag : NSObject
@property(nonatomic,strong) PIPItem *leader;
@property(nonatomic) CGPoint start;
@property(nonatomic) CGPoint pointer;
@property(nonatomic) CGPoint grab;
@property(nonatomic) CGPoint velocity;
@property(nonatomic) CFTimeInterval timestamp;
@property(nonatomic) BOOL moved;
@end
@implementation PIPDrag
@end
@interface PIPResize : NSObject
@property(nonatomic,strong) PIPItem *leader;
@property(nonatomic) OCTPIPStackAlignment alignment;
@property(nonatomic) CGPoint start;
@property(nonatomic) CGPoint fixedPoint;
@property(nonatomic) CGFloat initialMax;
@property(nonatomic) BOOL previousScaling;
@end
@implementation PIPResize
@end
@interface PIPMove : NSObject
@property(nonatomic) CGPoint start;
@property(nonatomic) CGPoint control;
@property(nonatomic,strong) PIPAnchor *end;
@property(nonatomic) CFTimeInterval startTime;
@property(nonatomic) double duration;
@property(nonatomic) BOOL finished;
@property(nonatomic) BOOL awaitingHost;
@end
@implementation PIPMove
@end

@interface PIPView : NSView
@property(nonatomic,weak) OCTPIPStack *stack;
@property(nonatomic) BOOL controlSurface;
@property(nonatomic,strong) NSTrackingArea *area;
@property(nonatomic,strong) id localMonitor;
@property(nonatomic,strong) id globalMonitor;
@property(nonatomic,copy) NSString *pressedControl;
@property(nonatomic) CGPoint down;
@property(nonatomic) BOOL dragging;
@property(nonatomic) BOOL resizing;
@property(nonatomic,strong) NSCursor *activeCursor;
@property(nonatomic,copy) NSArray<NSDictionary *> *tooltips;
- (void)refreshPointer;
- (void)stopMonitoring;
@end

struct PIPDisplayState : std::enable_shared_from_this<PIPDisplayState> {
  __weak OCTPIPStack *owner;
  std::atomic_bool pending{false};
  std::atomic_bool running{true};
};

@interface OCTPIPStack () {
  OCTPIPStackCallbacks *_callbacks;
  OCTPIPStackControlText *_text;
  NSImage *_placementImage;
  NSMutableDictionary<OCTPIPStackIdentity *,PIPItem *> *_items;
  NSMutableDictionary<OCTPIPStackIdentity *,PIPItem *> *_exits;
  NSMutableArray<OCTPIPStackIdentity *> *_order;
  NSMutableDictionary<NSString *,PIPHost *> *_hosts;
  NSMutableDictionary<NSString *,NSDictionary *> *_preferences;
  NSSet<NSString *> *_suppressed;
  NSString *_activeGroup;
  NSPanel *_panel,*_controlPanel;
  PIPView *_view,*_controlView;
  PIPHost *_host;
  PIPAnchor *_target;
  PIPDrag *_drag;
  PIPResize *_resize;
  PIPMove *_move;
  NSMutableArray *_observers;
  CGFloat _maxSize;
  BOOL _customMax,_floating,_closed,_motion,_hostFollow,_pendingLayout,_pendingAnimated,_reconciling;
  BOOL _applyingFrames,_deferredHostChange;
  NSInteger _pendingRole;
  OCTPIPStackIdentity *_pendingPlacement;
  OCTPIPStackSpring *_hostSpring;
  OCTPIPStackAlignment _preferred;
  CGSize _expandedSize,_lastRestSize;
  CGRect _frozenEnvelope;
  BOOL _hasFrozenEnvelope;
  CGPoint _previousHostAnchor;
  CVDisplayLinkRef _displayLink;
  std::shared_ptr<PIPDisplayState> _linkState;
  CFTimeInterval _lastFrame,_lastSample,_lastColorChange;
  NSUInteger _lightSamples;
  BOOL _contrastSampling;
  double _white,_reveal;
  CALayer *_fog,*_blurBackdrop,*_blurShape,*_shadowBackdrop,*_shadowShape,*_hideButton,*_placeButton,*_hideIcon,*_placeIcon;
  CATextLayer *_hideText;
  CIFilter *_hideColor,*_placeColor;
  CGRect _hideFrame,_placeFrame,_fogFrame;
  BOOL _controlsVisible;
  BOOL _usesVariableBlur;
  CGFloat _iconScale;
  CALayer *_blurMask,*_shadowMask;
  NSString *_hoverControl;
}
- (BOOL)validItem:(PIPItem *)item;
- (BOOL)validHost:(PIPHost *)host visible:(BOOL)visible;
- (BOOL)ensurePanels:(NSError **)error;
- (void)attachHost:(PIPHost *)host;
- (PIPHost *)defaultHost;
- (void)hostChanged:(NSNotification *)note;
- (NSArray<PIPItem *> *)orderedItems;
- (NSArray<PIPItem *> *)motionItems;
- (void)layoutAnimated:(BOOL)animated;
- (void)applyFrames:(BOOL)expanded;
- (void)updateOffsets;
- (CGPoint)currentAnchor;
- (void)configureSprings:(nullable PIPItem *)leader dragging:(BOOL)dragging programmatic:(BOOL)programmatic;
- (void)startMotion;
- (void)startDisplayLink;
- (void)stopDisplayLink;
- (void)frameAt:(double)timestamp state:(std::shared_ptr<PIPDisplayState>)state;
- (void)updateControls:(CGRect)lead visible:(BOOL)visible above:(BOOL)above docked:(BOOL)docked;
- (void)updateHover:(CGPoint)point;
- (nullable NSString *)controlAt:(CGPoint)point;
- (void)performControl:(NSString *)control;
- (nullable PIPItem *)itemAt:(CGPoint)point;
- (nullable PIPItem *)resizeAt:(CGPoint)point alignment:(OCTPIPStackAlignment *)alignment;
- (BOOL)beginResize:(CGPoint)point screen:(CGPoint)screen;
- (void)resizeTo:(CGPoint)point;
- (void)endResize;
- (BOOL)beginDrag:(CGPoint)point;
- (void)dragTo:(CGPoint)point;
- (void)endDrag;
- (CGRect)passthroughScreenRect;
- (BOOL)acceptsPoint:(CGPoint)point controlSurface:(BOOL)control;
- (void)refreshCursorRects;
- (void)updateContrast:(double)timestamp force:(BOOL)force;
- (void)interrupt:(nullable PIPItem *)item error:(NSError *)error;
- (NSArray<PIPAnchor *> *)anchorsForHost:(PIPHost *)host;
- (PIPAnchor *)anchorForHost:(PIPHost *)host alignment:(OCTPIPStackAlignment)alignment;
- (CGPoint)snapPoint:(CGPoint)point;
- (void)finishExit:(PIPItem *)item;
- (void)beginMove:(PIPAnchor *)anchor from:(CGPoint)start;
- (void)commitAnchor:(PIPAnchor *)anchor;
- (BOOL)installItem:(PIPItem *)item animated:(BOOL)animated error:(NSError **)error;
- (BOOL)layoutEmbeddedItem:(PIPItem *)item error:(NSError **)error;
- (void)hideFromMenu:(NSMenuItem *)sender;
- (void)setReveal:(double)value animated:(BOOL)animated;
- (NSCursor *)resizeCursor:(OCTPIPStackAlignment)alignment;
- (void)reconcilePendingHost;
- (BOOL)canRequestAccessory;
@end

@implementation OCTPIPStack
- (instancetype)initWithCallbacks:(OCTPIPStackCallbacks *)callbacks controlText:(OCTPIPStackControlText *)text placementAssetURL:(NSURL *)url error:(NSError **)error {
  if(!(self=[super init]))return nil;
  _closed=YES;
  if(!NSThread.isMainThread){PIPFail(error,@"PiP presentation requires the main thread.");return nil;}
  if(!callbacks.checkIdentity||!callbacks.checkHost||!callbacks.shouldShowGroup||!callbacks.focusRequested||!callbacks.maxDisplaySizeRequested||!callbacks.hideGroupsRequested||!callbacks.returnToMainRequested||!callbacks.hostChanged||!callbacks.anchorCommitted||!callbacks.restSizeChanged||!callbacks.lookTargetChanged||!callbacks.interrupted){PIPFail(error,@"PiP requires complete native identity, host and client action callbacks.");return nil;}
  if(!text.closeTooltip.length||!text.hide.length||!text.hideForGroup.length||!text.hideForAllGroups.length||!text.placementTooltip.length||!url.isFileURL){PIPFail(error,@"PiP control text and the owned placement asset are required.");return nil;}
  NSData *data=[NSData dataWithContentsOfURL:url options:NSDataReadingMappedIfSafe error:error];
  if(!data)return nil;
  unsigned char digest[CC_SHA256_DIGEST_LENGTH];CC_SHA256(data.bytes,(CC_LONG)data.length,digest);
  NSMutableString *hash=[NSMutableString string];for(unsigned char byte:digest)[hash appendFormat:@"%02x",byte];
  if(data.length!=3767||![hash isEqual:@"8979fa7ce7b7dabf97ab3e73c5e7652cbf5564a94ed7cd213fa070eebc2519ea"]){PIPFail(error,@"PiP placement asset integrity mismatch.");return nil;}
  _placementImage=[[NSImage alloc] initWithData:data];
  if(!_placementImage||!PIPCGImage(_placementImage)){PIPFail(error,@"PiP placement asset cannot be decoded.");return nil;}
  _callbacks=[OCTPIPStackCallbacks new];
  _callbacks.checkIdentity=callbacks.checkIdentity;_callbacks.checkHost=callbacks.checkHost;_callbacks.shouldShowGroup=callbacks.shouldShowGroup;
  _callbacks.focusRequested=callbacks.focusRequested;_callbacks.maxDisplaySizeRequested=callbacks.maxDisplaySizeRequested;
  _callbacks.hideGroupsRequested=callbacks.hideGroupsRequested;_callbacks.returnToMainRequested=callbacks.returnToMainRequested;_callbacks.sendToAccessoryRequested=callbacks.sendToAccessoryRequested;
  _callbacks.hostChanged=callbacks.hostChanged;_callbacks.anchorCommitted=callbacks.anchorCommitted;_callbacks.restSizeChanged=callbacks.restSizeChanged;_callbacks.lookTargetChanged=callbacks.lookTargetChanged;_callbacks.interrupted=callbacks.interrupted;
  _text=[OCTPIPStackControlText new];_text.closeTooltip=text.closeTooltip;_text.hide=text.hide;_text.hideForGroup=text.hideForGroup;_text.hideForAllGroups=text.hideForAllGroups;_text.placementTooltip=text.placementTooltip;
  _items=[NSMutableDictionary dictionary];_exits=[NSMutableDictionary dictionary];_order=[NSMutableArray array];_hosts=[NSMutableDictionary dictionary];_preferences=[NSMutableDictionary dictionary];_suppressed=[NSSet set];_observers=[NSMutableArray array];
  _maxSize=200;_preferred=OCTPIPStackTopRight;_floating=YES;_white=1;_pendingRole=-1;_closed=NO;
  __weak OCTPIPStack *weak=self;
  for(NSNotificationName name in @[NSWindowDidBecomeKeyNotification,NSWindowDidBecomeMainNotification,NSApplicationDidBecomeActiveNotification,NSApplicationDidChangeScreenParametersNotification,NSWindowDidMoveNotification,NSWindowDidResizeNotification,NSWindowDidChangeScreenNotification,NSWindowDidChangeBackingPropertiesNotification,NSWindowDidMiniaturizeNotification,NSWindowDidDeminiaturizeNotification,NSWindowWillCloseNotification]){
    [_observers addObject:[NSNotificationCenter.defaultCenter addObserverForName:name object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note){[weak hostChanged:note];}]];
  }
  return self;
}
- (BOOL)validItem:(PIPItem *)item {
  if(_closed||!item||item.detached||(_items[item.identity]!=item&&_exits[item.identity]!=item))return NO;
  BOOL valid=_callbacks.checkIdentity(item.identity);
  return valid&&!_closed&&!item.detached&&(_items[item.identity]==item||_exits[item.identity]==item);
}
- (BOOL)validHost:(PIPHost *)host visible:(BOOL)visible {
  NSWindow *w=host.window;
  if(_closed||!host||_hosts[host.identifier]!=host||!w||!w.contentView||w==_panel||w==_controlPanel||w.miniaturized||(visible&&!w.visible))return NO;
  BOOL valid=_callbacks.checkHost(host.identifier,host.generation,w);
  return valid&&!_closed&&_hosts[host.identifier]==host&&host.window==w&&w.contentView&&!w.miniaturized&&(!visible||w.visible);
}
- (BOOL)registerHost:(NSString *)identifier generation:(NSString *)generation window:(NSWindow *)window anchorContentRect:(CGRect)rect passthroughRect:(CGRect)passthrough anchors:(NSArray<OCTPIPStackHostAnchor *> *)anchors scope:(OCTPIPStackScope)scope role:(OCTPIPStackHostRole)role homeAvailable:(BOOL)homeAvailable animated:(BOOL)animated spring:(OCTPIPStackSpring *)spring error:(NSError **)error {
  PIPMain();if(_closed)return PIPFail(error,@"PiP stack is closed.");
  if(!identifier.length||!generation.length||!window||!window.contentView||window==_panel||window==_controlPanel||window.miniaturized||!PIPRect(rect)||!PIPPoint(passthrough.origin)||!std::isfinite(passthrough.size.width)||!std::isfinite(passthrough.size.height)||passthrough.size.width<0||passthrough.size.height<0||scope<OCTPIPStackActive||scope>OCTPIPStackAll||role<OCTPIPStackMain||role>OCTPIPStackAccessory)return PIPFail(error,@"Invalid native PiP host or geometry.");
  if(spring&&(!std::isfinite(spring.mass)||spring.mass<=0||!std::isfinite(spring.stiffness)||spring.stiffness<=0||!std::isfinite(spring.damping)||spring.damping<0))return PIPFail(error,@"Invalid host animation spring.");
  NSMutableArray *copied=[NSMutableArray array];
  for(OCTPIPStackHostAnchor *a in anchors){if(!PIPPoint(a.contentPoint)||a.alignment<0||a.alignment>5)return PIPFail(error,@"Invalid native PiP host anchor.");[copied addObject:[[OCTPIPStackHostAnchor alloc] initWithPoint:a.contentPoint alignment:a.alignment]];}
  PIPHost *old=_hosts[identifier];
  if(old&&(![old.generation isEqual:generation]||old.window!=window))return PIPFail(error,@"Remove the previous native host generation before replacement.");
  if(!_callbacks.checkHost(identifier,generation,window)||_closed)return PIPFail(error,@"Native PiP host is no longer admitted.");
  PIPHost *h=old?:[PIPHost new];h.identifier=identifier;h.generation=generation;h.window=window;h.contentRect=rect;h.passthrough=passthrough;h.anchors=copied;h.scope=scope;h.role=role;h.homeAvailable=homeAvailable;_hosts[identifier]=h;
  if(spring){_hostSpring=[OCTPIPStackSpring new];_hostSpring.mass=spring.mass;_hostSpring.stiffness=spring.stiffness;_hostSpring.damping=spring.damping;}
  if(!_host)[self attachHost:[self defaultHost]];
  if(_move&&_move.end.host==h){
    PIPAnchor *next=[self anchorForHost:h alignment:_move.end.alignment];
    if(_move.awaitingHost){_move.awaitingHost=NO;_move.start=[self currentAnchor];_move.startTime=CACurrentMediaTime();_move.end=next;_move.control=CGPointMake((_move.start.x+next.point.x)/2,2*(std::max(_move.start.y,next.point.y)+72)-(_move.start.y+next.point.y)/2);[self attachHost:h];[self startMotion];}
    else if(PIPLength(PIPSub(next.point,_move.end.point))>=.5){double remaining=std::max(1./60,_move.startTime+_move.duration-CACurrentMediaTime());_move.start=[self currentAnchor];_move.control=PIPMul(PIPAdd(_move.start,next.point),.5);_move.end=next;_move.duration=remaining;_move.startTime=CACurrentMediaTime();_move.finished=NO;}
    else _move.end=next;
  }
  if(h==_host){[self refreshVisibilityAnimated:animated];[self layoutAnimated:animated];}
  [self reconcilePendingHost];return !_closed;
}
- (void)removeHost:(NSString *)identifier generation:(NSString *)generation {
  PIPMain();PIPHost *h=_hosts[identifier];if(!h||![h.generation isEqual:generation])return;
  for(PIPItem *item in _items.allValues.copy)if(item.embeddedHost==h)[self detachEmbeddedView:item.embeddedID generation:item.embeddedGeneration];
  [_hosts removeObjectForKey:identifier];
  if(_move.end.host==h)_move=nil;
  if(_target.host==h)_target=nil;
  if(h==_host){_drag=nil;_resize=nil;_motion=NO;_hostFollow=NO;_hasFrozenEnvelope=NO;[self attachHost:nil];[_panel orderOut:nil];[_controlPanel orderOut:nil];[self stopDisplayLink];[self interrupt:nil error:PIPError(@"The native PiP host was removed.")];}
}
- (PIPHost *)defaultHost {
  NSArray *windows=@[NSApp.keyWindow?:NSNull.null,NSApp.mainWindow?:NSNull.null];
  NSMutableArray *ordered=[windows mutableCopy];[ordered addObjectsFromArray:NSApp.orderedWindows];
  for(id w in ordered){if(![w isKindOfClass:NSWindow.class]||[w isKindOfClass:NSPanel.class]||[w level]!=NSNormalWindowLevel)continue;
    for(PIPHost *h in _hosts.allValues)if(h.role==OCTPIPStackMain&&h.window==w&&[self validHost:h visible:YES])return h;
  }
  return nil;
}
- (void)attachHost:(PIPHost *)host {
  if(host&&![self validHost:host visible:NO])return;
  if(_host==host)return;
  NSRect frame=_panel.frame,controls=_controlPanel.frame;
  [_panel.parentWindow removeChildWindow:_panel];[_controlPanel.parentWindow removeChildWindow:_controlPanel];_host=host;_hostFollow=NO;
  if(host&&_panel){NSWindow *w=host.window;_panel.level=w.level;_controlPanel.level=w.level;[w addChildWindow:_panel ordered:NSWindowAbove];[w addChildWindow:_controlPanel ordered:NSWindowAbove];[_panel setFrame:frame display:NO];[_controlPanel setFrame:controls display:NO];}
  _callbacks.hostChanged(host.identifier);
}
- (NSArray<PIPAnchor *> *)anchorsForHost:(PIPHost *)host {
  if(![self validHost:host visible:YES])return @[];
  NSMutableArray *result=[NSMutableArray array];
  for(OCTPIPStackHostAnchor *a in host.anchors)[result addObject:[PIPAnchor at:[host screenPoint:a.contentPoint] alignment:a.alignment host:host]];
  if(result.count)return result;
  CGRect r=CGRectInset(host.screenRect,24,24);
  [result addObject:[PIPAnchor at:CGPointMake(CGRectGetMinX(r),CGRectGetMinY(r)) alignment:OCTPIPStackBottomLeft host:host]];
  [result addObject:[PIPAnchor at:CGPointMake(CGRectGetMaxX(r),CGRectGetMinY(r)) alignment:OCTPIPStackBottomRight host:host]];
  [result addObject:[PIPAnchor at:CGPointMake(CGRectGetMinX(r),CGRectGetMaxY(r)) alignment:OCTPIPStackTopLeft host:host]];
  [result addObject:[PIPAnchor at:CGPointMake(CGRectGetMaxX(r),CGRectGetMaxY(r)) alignment:OCTPIPStackTopRight host:host]];
  return result;
}
- (PIPAnchor *)anchorForHost:(PIPHost *)host alignment:(OCTPIPStackAlignment)alignment {
  NSArray *anchors=[self anchorsForHost:host];for(PIPAnchor *a in anchors)if(a.alignment==alignment)return a;
  return anchors.firstObject;
}
- (void)hostChanged:(NSNotification *)note {
  if(_closed||note.object==_panel||note.object==_controlPanel)return;
  if([note.name isEqual:NSWindowWillCloseNotification]){for(PIPHost *h in _hosts.allValues.copy)if(h.window==note.object)[self removeHost:h.identifier generation:h.generation];return;}
  if(_applyingFrames){_deferredHostChange=YES;return;}
  if(_host&&![self validHost:_host visible:NO]){[_panel orderOut:nil];[_controlPanel orderOut:nil];_contrastSampling=NO;[self stopDisplayLink];return;}
  if(!_host)[self attachHost:[self defaultHost]];
  if(_drag||_resize||_move){_pendingLayout=YES;_pendingAnimated=YES;return;}
  [self refreshVisibilityAnimated:NO];[self layoutAnimated:NO];
}
- (BOOL)layoutEmbeddedItem:(PIPItem *)item error:(NSError **)error {
  if(![self validItem:item]||!item.embeddedView||![self validHost:item.embeddedHost visible:NO]
     ||item.embeddedView.superview!=item.embeddedHost.window.contentView)return PIPFail(error,@"Embedded application lost its native host.");
  CGSize bounds=item.embeddedView.bounds.size;
  if(!PIPSize(bounds))return PIPFail(error,@"Embedded application has no visible rectangle.");
  double scale=std::min(bounds.width/item.sourceSize.width,bounds.height/item.sourceSize.height);
  CGSize size=CGSizeMake(item.sourceSize.width*scale,item.sourceSize.height*scale);
  BOOL show=item.embeddedVisible&&[self validHost:item.embeddedHost visible:YES];
  [CATransaction begin];[CATransaction setDisableActions:YES];
  item.scalesContent=YES;item.shadow.shadowOpacity=0;item.appearance.anchorPoint=CGPointMake(.5,.5);
  [item appearanceVisible:show animated:NO completion:nil];
  BOOL result=[item layout:CGRectMake((bounds.width-size.width)/2,(bounds.height-size.height)/2,size.width,size.height) error:error];
  [CATransaction commit];return result&&[self validItem:item];
}
- (BOOL)embedItem:(OCTPIPStackIdentity *)identity viewID:(NSString *)viewID generation:(NSString *)generation
          hostID:(NSString *)hostID hostGeneration:(NSString *)hostGeneration window:(NSWindow *)window
     contentRect:(CGRect)rect error:(NSError **)error {
  PIPMain();PIPItem *item=_items[identity];PIPHost *host=_hosts[hostID];
  if(![self validItem:item]||item.imageItem||!item.attachment||!viewID.length||!generation.length
     ||![host.generation isEqual:hostGeneration]||host.window!=window||![self validHost:host visible:YES]
     ||!PIPRect(rect)||!CGRectContainsRect(window.contentView.bounds,rect))return PIPFail(error,@"Embedded application requires its exact live source, host and visible rectangle.");
  for(PIPItem *other in _items.allValues)if(other!=item&&[other.embeddedID isEqual:viewID])return PIPFail(error,@"Embedded view already belongs to another application.");
  if(item.embeddedView&&(![item.embeddedID isEqual:viewID]||![item.embeddedGeneration isEqual:generation]||item.embeddedHost!=host))
    return PIPFail(error,@"Detach the previous embedded owner before replacement.");
  if(!item.embeddedView){
    item.embeddedView=[[PIPEmbeddedView alloc] initWithFrame:rect];item.embeddedView.wantsLayer=YES;
    item.embeddedView.layer.masksToBounds=YES;item.embeddedView.layer.backgroundColor=NSColor.clearColor.CGColor;
    item.embeddedID=viewID;item.embeddedGeneration=generation;item.embeddedHost=host;
    if(_drag.leader==item)_drag=nil;if(_resize.leader==item){_resize=nil;_hasFrozenEnvelope=NO;}
    [window.contentView addSubview:item.embeddedView positioned:NSWindowAbove relativeTo:nil];
    [item.root removeFromSuperlayer];[item.embeddedView.layer addSublayer:item.root];
  }else item.embeddedView.frame=rect;
  item.embeddedVisible=YES;
  if(![self layoutEmbeddedItem:item error:error]){[self detachEmbeddedView:viewID generation:generation];return NO;}
  [self refreshVisibilityAnimated:NO];return [self validItem:item];
}
- (void)hideEmbeddedView:(NSString *)viewID generation:(NSString *)generation {
  PIPMain();if(_closed)return;
  for(PIPItem *item in _items.allValues.copy)if(item.embeddedView&&[item.embeddedID isEqual:viewID]&&[item.embeddedGeneration isEqual:generation]){
    item.embeddedVisible=NO;NSError *error=nil;
    if(![self layoutEmbeddedItem:item error:&error])[self invalidateIdentity:item.identity error:error?:PIPError(@"Embedded application is no longer current.")];
  }
}
- (void)detachEmbeddedView:(NSString *)viewID generation:(NSString *)generation {
  PIPMain();if(_closed)return;
  for(PIPItem *item in _items.allValues.copy)if(item.embeddedView&&[item.embeddedID isEqual:viewID]&&[item.embeddedGeneration isEqual:generation]){
    [item.root removeFromSuperlayer];[item.embeddedView removeFromSuperview];item.embeddedView=nil;item.embeddedHost=nil;
    item.embeddedID=nil;item.embeddedGeneration=nil;item.shadow.shadowOpacity=.16;item.scalesContent=YES;
    item.displaySize=PIPFit(item.sourceSize,item.maxSize);item.initialized=NO;
    [item appearanceVisible:NO animated:NO completion:nil];[_view.layer addSublayer:item.root];
  }
  [self refreshVisibilityAnimated:YES];
}
- (NSArray<PIPItem *> *)orderedItems {
  NSMutableArray *items=[NSMutableArray array];for(OCTPIPStackIdentity *identity in _order.copy){PIPItem *item=_items[identity];if(item&&!item.embeddedView)[items addObject:item];}return items;
}
- (NSArray<PIPItem *> *)motionItems {
  NSMutableArray *items=[[self orderedItems] mutableCopy];[items addObjectsFromArray:_exits.allValues];return items;
}
- (BOOL)installItem:(PIPItem *)item animated:(BOOL)animated error:(NSError **)error {
  if(![self ensurePanels:error])return NO;
  if(!_callbacks.checkIdentity(item.identity)||_closed)return PIPFail(error,@"PiP item identity is no longer admitted.");
  if(_items[item.identity]||_exits[item.identity])return PIPFail(error,@"PiP presentation generation already exists.");
  _items[item.identity]=item;[_order addObject:item.identity];[_view.layer addSublayer:item.root];
  __weak OCTPIPStack *weak=self;__weak PIPItem *weakItem=item;
  item.valid=^BOOL{OCTPIPStack *stack=weak;return stack&&[stack validItem:weakItem];};
  if(item.factory){
    item.attachment=item.factory(item.content,item.displaySize,error);item.factory=nil;
    if(!item.attachment||![self validItem:item]){[_items removeObjectForKey:item.identity];[_order removeObject:item.identity];[item detach];if(error&&!*error)*error=PIPError(@"PiP native attachment was not created for the current identity.");return NO;}
  }
  [self refreshVisibilityAnimated:animated&&_order.count>1];return [self validItem:item];
}
- (BOOL)publishRemoteItem:(OCTPIPStackIdentity *)identity sourceSize:(CGSize)size groupID:(NSString *)groupID batchID:(NSString *)batchID factory:(OCTPIPStackAttachmentFactory)factory animated:(BOOL)animated error:(NSError **)error {
  PIPMain();if(_closed||!identity||!PIPSize(size)||!groupID.length||!batchID.length||!factory)return PIPFail(error,@"Invalid native PiP publication.");
  PIPItem *item=[[PIPItem alloc] initWithSize:size max:_maxSize image:nil];if(!item)return PIPFail(error,@"PiP item layers are unavailable.");
  item.identity=identity;item.groupID=groupID;item.batchID=batchID;item.factory=factory;return [self installItem:item animated:animated error:error];
}
- (BOOL)publishImageItem:(OCTPIPStackIdentity *)identity image:(NSImage *)image groupID:(NSString *)groupID batchID:(NSString *)batchID animated:(BOOL)animated error:(NSError **)error {
  PIPMain();if(_closed||!identity||!PIPSize(image.size)||!PIPCGImage(image)||!groupID.length||!batchID.length)return PIPFail(error,@"Invalid native PiP image publication.");
  PIPItem *item=[[PIPItem alloc] initWithSize:image.size max:_customMax?_maxSize:400 image:image];if(!item)return PIPFail(error,@"PiP image layers are unavailable.");
  item.identity=identity;item.groupID=groupID;item.batchID=batchID;return [self installItem:item animated:animated error:error];
}
- (BOOL)updateImage:(NSImage *)image identity:(OCTPIPStackIdentity *)identity error:(NSError **)error {
  PIPMain();PIPItem *item=_items[identity];if(![self validItem:item]||!item.imageItem||!PIPSize(image.size)||!PIPCGImage(image))return PIPFail(error,@"PiP image update is invalid or stale.");
  [CATransaction begin];[CATransaction setDisableActions:YES];item.image=image;item.sourceSize=image.size;item.displaySize=PIPFit(image.size,item.maxSize);item.imageLayer.contents=(__bridge id)PIPCGImage(image);[CATransaction commit];[self layoutAnimated:YES];return [self validItem:item];
}
- (BOOL)applySourceSize:(CGSize)size identity:(OCTPIPStackIdentity *)identity phase:(OCTPIPStackSourcePhase)phase prepare:(OCTPIPStackSourcePrepare)prepare error:(NSError **)error {
  PIPMain();PIPItem *item=_items[identity];if(![self validItem:item]||item.imageItem||!PIPSize(size)||!prepare||phase<0||phase>OCTPIPStackSourceReplacement)return PIPFail(error,@"PiP source resize is invalid or stale.");
  CGSize display=PIPFit(size,item.maxSize);
  if(!prepare(display,error))return NO;
  if(_items[identity]!=item||![self validItem:item])return PIPFail(error,@"PiP source resize was superseded during native preparation.");
  item.sourceSize=size;
  if(item.embeddedView)return [self layoutEmbeddedItem:item error:error];
  if(phase!=OCTPIPStackSourceProgress){item.displaySize=display;for(PIPItem *other in [self motionItems])other.scalesContent=NO;[self layoutAnimated:YES];}
  return [self validItem:item];
}
- (void)finishExit:(PIPItem *)item {
  if(_exits[item.identity]!=item)return;
  [_exits removeObjectForKey:item.identity];void(^completion)(void)=item.removalCompletion;item.removalCompletion=nil;[item detach];
  if(!_closed)[self layoutAnimated:YES];if(completion)completion();
}
- (void)removeItem:(OCTPIPStackIdentity *)identity animated:(BOOL)animated completion:(void (^)(void))completion {
  PIPMain();if(!identity){if(completion)completion();return;}
  PIPItem *item=_items[identity];if(!item){PIPItem *exit=_exits[identity];if(exit&&!animated)[self finishExit:exit];if(completion)completion();return;}
  if(item.embeddedView){[item.embeddedView removeFromSuperview];item.embeddedView=nil;item.embeddedHost=nil;animated=NO;}
  [_items removeObjectForKey:identity];[_order removeObject:identity];_exits[identity]=item;item.removalCompletion=completion;
  if(_drag.leader==item)_drag=nil;if(_resize.leader==item){_resize=nil;_hasFrozenEnvelope=NO;}
  if([_pendingPlacement isEqual:identity]){_pendingPlacement=nil;_pendingRole=-1;}
  [item stopCompletion];
  if(!animated||!item.contributes||!_panel.visible||!_host){[self finishExit:item];return;}
  __weak OCTPIPStack *weak=self;__weak PIPItem *weakItem=item;
  [item appearanceVisible:NO animated:YES completion:^{[weak finishExit:weakItem];}];
  [self layoutAnimated:YES];
}
- (void)showCompletionForItem:(OCTPIPStackIdentity *)identity applicationIcon:(NSImage *)icon {PIPMain();PIPItem *item=_items[identity];if([self validItem:item])[item showCompletion:icon];}
- (BOOL)noteInteraction:(OCTPIPStackIdentity *)identity {
  PIPMain();PIPItem *item=_items[identity];if(![self validItem:item])return NO;[item stopCompletion];[_order removeObject:identity];[_order insertObject:identity atIndex:0];[self refreshVisibilityAnimated:NO];return [self validItem:item];
}
- (void)setActiveGroup:(NSString *)groupID suppressedGroups:(NSSet<NSString *> *)suppressed {
  PIPMain();if(_closed)return;
  if(![_activeGroup isEqual:groupID]){
    if(_activeGroup)_preferences[_activeGroup]=@{@"alignment":@(_preferred),@"floating":@(_floating)};
    _activeGroup=[groupID copy];NSDictionary *p=groupID?_preferences[groupID]:nil;
    _preferred=p?(OCTPIPStackAlignment)[p[@"alignment"] integerValue]:OCTPIPStackTopRight;_floating=p?[p[@"floating"] boolValue]:!_host.homeAvailable;
  }
  _suppressed=[suppressed copy];[self refreshVisibilityAnimated:NO];
}
- (void)refreshVisibilityAnimated:(BOOL)animated {
  PIPMain();if(_closed)return;
  for(PIPItem *item in _items.allValues.copy)if(item.embeddedView){NSError *error=nil;if(![self layoutEmbeddedItem:item error:&error])[self invalidateIdentity:item.identity error:error?:PIPError(@"Embedded source is no longer available.")];}
  BOOL hostVisible=[self validHost:_host visible:YES],all=_host.scope==OCTPIPStackAll;
  NSUInteger visibleCount=0;
  for(PIPItem *item in [self orderedItems]){
    if(![self validItem:item]){[self invalidateIdentity:item.identity error:PIPError(@"PiP presentation identity expired.")];continue;}
    BOOL show=hostVisible&&![_suppressed containsObject:item.groupID]&&(all||[_activeGroup isEqual:item.groupID])&&(_floating||all||_host.homeAvailable)&&_callbacks.shouldShowGroup(item.groupID);
    if(![self validItem:item])continue;
    show=show&&visibleCount<5;if(show)++visibleCount;
    if(item.visible!=show){__weak OCTPIPStack *weak=self;[item appearanceVisible:show animated:animated completion:^{if(!weak)return;[weak layoutAnimated:YES];}];}
  }
  [self layoutAnimated:animated];
}
- (BOOL)setMaxDisplaySize:(CGFloat)size error:(NSError **)error {
  PIPMain();if(_closed||!std::isfinite(size)||size<100||size>400)return PIPFail(error,@"PiP display size must be between 100 and 400 points.");
  _maxSize=size;_customMax=YES;for(PIPItem *item in [self motionItems]){item.maxSize=size;item.displaySize=PIPFit(item.sourceSize,size);}
  [self layoutAnimated:YES];return !_closed;
}
- (void)setPreferredAlignment:(OCTPIPStackAlignment)alignment floating:(BOOL)floating {
  PIPMain();if(_closed||alignment<0||alignment>5)return;_preferred=alignment;_floating=floating;
  if(_activeGroup)_preferences[_activeGroup]=@{@"alignment":@(alignment),@"floating":@(floating)};
  [self refreshVisibilityAnimated:YES];
}
- (CGPoint)snapPoint:(CGPoint)p {
  NSWindow *window=_host.window?:_panel;CGFloat scale=window.backingScaleFactor;
  if(!std::isfinite(scale)||scale<=0)return p;
  return CGPointMake(std::round(p.x*scale)/scale,std::round(p.y*scale)/scale);
}
- (CGPoint)currentAnchor {
  CGPoint sum=CGPointZero;NSUInteger count=0;
  for(PIPItem *item in [self orderedItems])if(item.contributes&&item.initialized){sum=PIPAdd(sum,PIPSub(item.origin,item.restOffset));++count;}
  if(count)return PIPMul(sum,1./count);
  PIPAnchor *anchor=_target?:[self anchorForHost:_host alignment:_preferred];return anchor?anchor.point:CGPointZero;
}
- (void)updateOffsets {
  OCTPIPStackAlignment alignment=_resize?_resize.alignment:(_target?_target.alignment:_preferred);
  CGPoint anchor=[self currentAnchor];NSUInteger index=0;CGRect rest=CGRectNull;
  [CATransaction begin];[CATransaction setDisableActions:YES];
  for(PIPItem *item in [self orderedItems]){
    item.restOffset=PIPOffset(item.displaySize,index,alignment);
    item.appearance.anchorPoint=CGPointMake(PIPAxis(alignment),PIPUp(alignment)?0:1);
    if(!item.initialized){item.origin=[self snapPoint:PIPAdd(anchor,item.restOffset)];item.target=item.origin;item.velocity=CGPointZero;item.initialized=YES;}
    if(item.contributes){rest=CGRectUnion(rest,(CGRect){item.restOffset,item.displaySize});++index;}
  }
  [CATransaction commit];CGSize size=CGRectIsNull(rest)?CGSizeZero:rest.size;
  if(!CGSizeEqualToSize(size,_lastRestSize)){_lastRestSize=size;_callbacks.restSizeChanged(size);}
}
- (void)configureSprings:(PIPItem *)leader dragging:(BOOL)dragging programmatic:(BOOL)programmatic {
  NSArray *items=[self orderedItems];NSUInteger lead=[items indexOfObjectIdenticalTo:leader];if(lead==NSNotFound)lead=0;
  for(NSUInteger i=0;i<items.count;++i){PIPItem *item=items[i];double distance=std::abs((double)i-(double)lead);if(programmatic)distance*=1+.45*distance;
    item.leads=i==lead;
    if(i==lead){item.stiffness=dragging?900:320;item.damping=dragging?55:42;}
    else{item.stiffness=(dragging?260:150)/(1+.18*distance);item.damping=(dragging?32:30)/(1+.08*distance);}
    if(_hostSpring&&!dragging){item.stiffness=_hostSpring.stiffness/_hostSpring.mass;item.damping=_hostSpring.damping/_hostSpring.mass;}
  }
}
- (void)layoutAnimated:(BOOL)animated {
  if(_closed||!_panel)return;
  if(_reconciling){_pendingLayout=YES;_pendingAnimated|=animated;return;}
  if(_drag||_resize||_move){_pendingLayout=YES;_pendingAnimated|=animated;return;}
  _reconciling=YES;
  @try{
    if(![self validHost:_host visible:YES]){[_panel orderOut:nil];[_controlPanel orderOut:nil];_contrastSampling=NO;_motion=NO;[self stopDisplayLink];return;}
    PIPAnchor *anchor=[self anchorForHost:_host alignment:_target?_target.alignment:_preferred];
    if(!anchor)return;
    if(_hostFollow&&_motion){CGPoint delta=PIPSub(anchor.point,_previousHostAnchor);for(PIPItem *item in [self motionItems]){item.origin=PIPAdd(item.origin,delta);item.target=PIPAdd(item.target,delta);}}
    _target=anchor;_previousHostAnchor=anchor.point;_hostFollow=YES;
    [self updateOffsets];if(_closed)return;
    [self configureSprings:nil dragging:NO programmatic:NO];
    BOOL contributing=NO;
    for(PIPItem *item in [self motionItems]){if(!item.contributes)continue;contributing=YES;item.target=[self snapPoint:PIPAdd(anchor.point,item.restOffset)];if(!animated){item.origin=item.target;item.velocity=CGPointZero;}}
    if(!contributing){[_panel orderOut:nil];[_controlPanel orderOut:nil];_motion=NO;_contrastSampling=NO;[self stopDisplayLink];[self updateControls:CGRectZero visible:NO above:NO docked:YES];return;}
    if(animated)[self startMotion];else{_motion=NO;[self applyFrames:NO];}
  }@finally{
    _reconciling=NO;
    if(_pendingLayout&&!_closed&&!_drag&&!_resize&&!_move){BOOL next=_pendingAnimated;_pendingLayout=NO;_pendingAnimated=NO;__weak OCTPIPStack *weak=self;dispatch_async(dispatch_get_main_queue(),^{[weak layoutAnimated:next];});}
  }
}
- (void)applyFrames:(BOOL)expanded {
  if(_closed||_applyingFrames||![self validHost:_host visible:YES])return;
  NSArray *items=[self motionItems];CGRect content=CGRectNull;NSUInteger count=0;
  NSMutableArray *stale=[NSMutableArray array];
  for(PIPItem *item in items){if(![self validItem:item]){[stale addObject:item];continue;}if(item.contributes){content=CGRectUnion(content,(CGRect){item.origin,item.displaySize});++count;}}
  for(PIPItem *item in stale){if(_exits[item.identity]==item)[self finishExit:item];else [self invalidateIdentity:item.identity error:PIPError(@"PiP identity expired during layout.")];}
  if(_closed||CGRectIsNull(content)||!count)return;
  double overscan=expanded?246:66;CGRect envelope;
  if(_resize&&_hasFrozenEnvelope)envelope=_frozenEnvelope;
  else if(_resize){
    NSUInteger followers=count?count-1:0;double w=std::max(content.size.width+2*overscan,400.+28*followers+2*overscan),h=std::max(content.size.height+2*overscan,400.+22*followers+2*overscan);
    CGPoint fixed=_resize.fixedPoint;double axis=PIPAxis(_resize.alignment);
    envelope=CGRectMake(fixed.x-w*axis+overscan*(2*axis-1),PIPUp(_resize.alignment)?fixed.y-overscan:fixed.y-h+overscan,w,h);
    envelope=CGRectUnion(envelope,CGRectInset(content,-overscan,-overscan));
  }else{
    CGSize size=CGSizeMake(content.size.width+2*overscan,content.size.height+2*overscan);
    if(expanded){_expandedSize=CGSizeMake(std::max(_expandedSize.width,size.width),std::max(_expandedSize.height,size.height));size=_expandedSize;}else _expandedSize=CGSizeZero;
    envelope=CGRectMake(CGRectGetMidX(content)-size.width/2,CGRectGetMidY(content)-size.height/2,size.width,size.height);
  }
  if(!_resize||!_hasFrozenEnvelope){
    CGPoint origin=[self snapPoint:envelope.origin],edge=[self snapPoint:CGPointMake(CGRectGetMaxX(envelope),CGRectGetMaxY(envelope))];envelope=(CGRect){origin,CGSizeMake(edge.x-origin.x,edge.y-origin.y)};
    NSScreen *screen=_host.window.screen;
    if(screen){CGRect visible=screen.visibleFrame;envelope.origin.x=std::min(std::max(envelope.origin.x,CGRectGetMinX(visible)),std::max(CGRectGetMinX(visible),CGRectGetMaxX(visible)-envelope.size.width));envelope.origin.y=std::min(std::max(envelope.origin.y,CGRectGetMinY(visible)),std::max(CGRectGetMinY(visible),CGRectGetMaxY(visible)-envelope.size.height));}
    envelope.origin.x=std::floor(envelope.origin.x);envelope.origin.y=std::floor(envelope.origin.y);
  }
  _applyingFrames=YES;NSMutableArray *failures=[NSMutableArray array];
  @try{
    [CATransaction begin];[CATransaction setDisableActions:YES];
    @try{
      [_panel setFrame:envelope display:NO];[_controlPanel setFrame:_panel.frame display:NO];envelope=_panel.frame;
      if(_resize){_frozenEnvelope=envelope;_hasFrozenEnvelope=YES;}
      CGRect bounds=(CGRect){CGPointZero,envelope.size};_view.frame=bounds;_controlView.frame=bounds;
      NSUInteger index=0;PIPItem *lead=nil;CGFloat top=-CGFLOAT_MAX;
      for(PIPItem *item in items){
        if(item.detached||(_items[item.identity]!=item&&_exits[item.identity]!=item))continue;
        CGRect local=(CGRect){PIPSub(item.origin,envelope.origin),item.displaySize};NSError *failure=nil;
        if(![item layout:local error:&failure]){[failures addObject:@[item,failure?:PIPError(@"Native PiP attachment layout failed.")]];continue;}
        if(_items[item.identity]==item){item.root.zPosition=item.contributes?(CGFloat)(count-index)+(item.leads?count+1:0):0;if(item.contributes)++index;}
        if(item.visible&&item.contributes&&CGRectGetMaxY(local)>top){top=CGRectGetMaxY(local);lead=item;}
      }
      [self updateControls:lead?lead.frame:CGRectZero visible:lead!=nil above:count>1 docked:!expanded];
    }@finally{[CATransaction commit];}
    if(!_closed&&[self validHost:_host visible:YES]){[_panel orderFront:nil];[_controlPanel orderWindow:NSWindowAbove relativeTo:_panel.windowNumber];}
  }@finally{_applyingFrames=NO;}
  for(NSArray *failure in failures)[self interrupt:failure[0] error:failure[1]];
  [self refreshCursorRects];
  if(_deferredHostChange){_deferredHostChange=NO;[self hostChanged:nil];}
}
- (void)startMotion {
  if(_closed)return;_motion=YES;[self startDisplayLink];[self applyFrames:YES];
}
- (void)startDisplayLink {
  if(_closed||_displayLink)return;
  CVDisplayLinkRef link=NULL;CVReturn result=CVDisplayLinkCreateWithActiveCGDisplays(&link);
  if(result!=kCVReturnSuccess||!link){[self interrupt:nil error:PIPError(@"Native PiP display link is unavailable.")];return;}
  _linkState=std::make_shared<PIPDisplayState>();_linkState->owner=self;_displayLink=link;
  result=CVDisplayLinkSetOutputCallback(link,[](CVDisplayLinkRef,const CVTimeStamp*,const CVTimeStamp *output,CVOptionFlags,CVOptionFlags*,void *context)->CVReturn{
    auto state=static_cast<PIPDisplayState*>(context)->shared_from_this();
    if(!state->running.load()||state->pending.exchange(true))return kCVReturnSuccess;
    double timestamp=(double)output->hostTime/CVGetHostClockFrequency();
    dispatch_async(dispatch_get_main_queue(),^{state->pending.store(false);if(state->running.load()){OCTPIPStack *owner=state->owner;if(owner)[owner frameAt:timestamp state:state];}});
    return kCVReturnSuccess;
  },_linkState.get());
  if(result==kCVReturnSuccess)result=CVDisplayLinkStart(link);
  if(result!=kCVReturnSuccess){[self stopDisplayLink];[self interrupt:nil error:PIPError(@"Native PiP display link could not start.")];}
}
- (void)stopDisplayLink {
  if(_linkState)_linkState->running.store(false);
  CVDisplayLinkRef link=_displayLink;_displayLink=NULL;
  if(link){CVDisplayLinkStop(link);CVDisplayLinkRelease(link);}_linkState.reset();_lastFrame=0;
}
- (void)frameAt:(double)timestamp state:(std::shared_ptr<PIPDisplayState>)state {
  if(_closed||state!=_linkState||!state->running.load())return;
  if(![self validHost:_host visible:YES]){[_panel orderOut:nil];[_controlPanel orderOut:nil];[self stopDisplayLink];return;}
  double dt=_lastFrame?std::clamp(timestamp-_lastFrame,1./120,1./30):1./120;_lastFrame=timestamp;
  if(_motion&&!_resize){
    CGPoint anchor=_target?_target.point:[self currentAnchor];
    if(_drag){if(![self validItem:_drag.leader]){[self interrupt:_drag.leader error:PIPError(@"PiP drag identity expired.")];return;}anchor=PIPSub(PIPSub(_drag.pointer,_drag.grab),_drag.leader.restOffset);}
    else if(_move&&!_move.awaitingHost){
      double t=std::clamp((timestamp-_move.startTime)/_move.duration,0.,1.),q=t*t*(3-2*t),inverse=1-q;
      anchor=PIPAdd(PIPAdd(PIPMul(_move.start,inverse*inverse),PIPMul(_move.control,2*inverse*q)),PIPMul(_move.end.point,q*q));_move.finished=t>=1;
    }
    BOOL settled=YES;NSMutableArray *invalid=[NSMutableArray array];
    for(PIPItem *item in [self motionItems]){
      if(![self validItem:item]){[invalid addObject:item];continue;}if(!item.contributes)continue;
      item.target=PIPAdd(anchor,item.restOffset);CGPoint delta=PIPSub(item.target,item.origin);
      CGPoint acceleration=PIPSub(PIPMul(delta,item.stiffness),PIPMul(item.velocity,item.damping));item.velocity=PIPAdd(item.velocity,PIPMul(acceleration,dt));item.origin=PIPAdd(item.origin,PIPMul(item.velocity,dt));
      if(PIPLength(delta)<=.5&&PIPLength(item.velocity)<=2){item.origin=item.target;item.velocity=CGPointZero;}else settled=NO;
    }
    for(PIPItem *item in invalid)[self invalidateIdentity:item.identity error:PIPError(@"PiP identity expired during animation.")];
    if(_closed)return;
    if(settled&&!_drag&&(!_move||(_move.finished&&!_move.awaitingHost))){
      PIPAnchor *end=_move.end;_move=nil;_motion=NO;_hostSpring=nil;
      if(end)[self commitAnchor:end];[self applyFrames:NO];
      if(_pendingLayout){BOOL animated=_pendingAnimated;_pendingLayout=NO;_pendingAnimated=NO;[self layoutAnimated:animated];}
    }else [self applyFrames:YES];
  }
  if(!_motion&&!_resize)[self updateContrast:timestamp force:NO];
  if(!_motion&&!_contrastSampling)[self stopDisplayLink];
}
- (void)beginMove:(PIPAnchor *)end from:(CGPoint)start {
  if(!end||![self validHost:end.host visible:YES])return;
  _drag=nil;_resize=nil;_hasFrozenEnvelope=NO;_hostFollow=NO;
  PIPMove *move=[PIPMove new];move.start=start;move.end=end;move.startTime=CACurrentMediaTime();move.duration=.32;
  move.control=CGPointMake((start.x+end.point.x)/2,2*(std::max(start.y,end.point.y)+72)-(start.y+end.point.y)/2);
  move.awaitingHost=end.host!=_host&&(end.alignment==OCTPIPStackTopCenter||end.alignment==OCTPIPStackBottomCenter);
  _move=move;_target=end;[self updateOffsets];[self configureSprings:nil dragging:NO programmatic:YES];
  if(!move.awaitingHost)[self attachHost:end.host];[self startMotion];
}
- (void)commitAnchor:(PIPAnchor *)anchor {
  if(!anchor||![self validHost:anchor.host visible:YES])return;
  _target=anchor;_preferred=anchor.alignment;_previousHostAnchor=anchor.point;_hostFollow=YES;[self attachHost:anchor.host];
  if(_activeGroup)_preferences[_activeGroup]=@{@"alignment":@(_preferred),@"floating":@(_floating)};
  _callbacks.anchorCommitted(anchor.host.identifier,anchor.alignment);
}
- (BOOL)moveToHost:(NSString *)identifier generation:(NSString *)generation alignment:(OCTPIPStackAlignment)alignment error:(NSError **)error {
  PIPMain();PIPHost *host=_hosts[identifier];if(![host.generation isEqual:generation]||![self validHost:host visible:YES]||alignment<0||alignment>5)return PIPFail(error,@"The requested native PiP host is unavailable.");
  PIPAnchor *anchor=[self anchorForHost:host alignment:alignment];if(!anchor)return PIPFail(error,@"The requested native PiP host has no admitted anchor.");
  _pendingRole=-1;_pendingPlacement=nil;_floating=host.scope!=OCTPIPStackAll;[self beginMove:anchor from:[self currentAnchor]];return !_closed;
}
- (BOOL)moveFromScreenPoint:(CGPoint)start toScreenPoint:(CGPoint)end error:(NSError **)error {
  PIPMain();if(!PIPPoint(start)||!PIPPoint(end)||![self validHost:_host visible:YES])return PIPFail(error,@"PiP movement requires finite native screen coordinates and a current host.");
  [self beginMove:[PIPAnchor at:end alignment:_target?_target.alignment:_preferred host:_host] from:start];return !_closed;
}
- (BOOL)canRequestAccessory {
  if(_closed)return NO;
  NSUInteger matches=0;
  for(PIPHost *host in _hosts.allValues)if(host.role==OCTPIPStackAccessory&&[self validHost:host visible:YES])++matches;
  return matches==1||_callbacks.sendToAccessoryRequested!=nil;
}
- (void)reconcilePendingHost {
  if(!_pendingPlacement||_pendingRole<0)return;
  PIPItem *item=_items[_pendingPlacement];if(![self validItem:item]){_pendingPlacement=nil;_pendingRole=-1;return;}
  PIPHost *candidate=nil;NSUInteger matches=0;
  for(PIPHost *host in _hosts.allValues)if(host.role==_pendingRole&&[self validHost:host visible:YES]){candidate=host;++matches;}
  if(_pendingRole==OCTPIPStackMain){candidate=[self defaultHost];matches=candidate?1:0;}
  if(matches==1){NSError *failure=nil;if(![self moveToHost:candidate.identifier generation:candidate.generation alignment:_preferred error:&failure])[self interrupt:item error:failure];}
}
- (BOOL)ensurePanels:(NSError **)error {
  if(_panel&&_controlPanel)return YES;
  @try{
    _panel=[[NSPanel alloc] initWithContentRect:NSZeroRect styleMask:NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
    _controlPanel=[[NSPanel alloc] initWithContentRect:NSZeroRect styleMask:NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
    if(!_panel||!_controlPanel)return PIPFail(error,@"Native PiP panels could not be created.");
    _panel.title=@"Computer Use";_controlPanel.title=@"Computer Use Controls";
    for(NSPanel *panel in @[_panel,_controlPanel]){panel.opaque=NO;panel.backgroundColor=NSColor.clearColor;panel.hasShadow=NO;panel.level=NSNormalWindowLevel;panel.acceptsMouseMovedEvents=YES;panel.collectionBehavior=NSWindowCollectionBehaviorTransient|NSWindowCollectionBehaviorFullScreenAuxiliary;panel.hidesOnDeactivate=NO;panel.movableByWindowBackground=NO;panel.releasedWhenClosed=NO;}
    _view=[[PIPView alloc] initWithFrame:NSZeroRect];_view.stack=self;_panel.contentView=_view;
    _controlView=[[PIPView alloc] initWithFrame:NSZeroRect];_controlView.stack=self;_controlView.controlSurface=YES;_controlPanel.contentView=_controlView;
    _fog=[CALayer layer];_fog.zPosition=99999;_fog.hidden=YES;_fog.opacity=0;
    _blurBackdrop=PIPBackdrop();_shadowBackdrop=PIPBackdrop();[_fog addSublayer:_blurBackdrop];[_fog addSublayer:_shadowBackdrop];
    _blurShape=[CALayer layer];_blurShape.backgroundColor=NSColor.blackColor.CGColor;
    id shapeBlur=PIPFilter(@"gaussianBlur");[shapeBlur setValue:@14 forKey:@"inputRadius"];_blurShape.filters=@[shapeBlur];
    if(@available(macOS 26.0,*)){
      _usesVariableBlur=YES;_blurShape.name=[@"remote-hosted-pip-control-blur-mask-" stringByAppendingString:NSUUID.UUID.UUIDString];
      id blur=PIPFilter(@"variableBlur");[blur setValue:@6 forKey:@"inputRadius"];[blur setValue:@YES forKey:@"inputFade"];[blur setValue:@YES forKey:@"inputNormalizeEdges"];[blur setValue:_blurShape.name forKey:@"inputSourceSublayerName"];
      _blurBackdrop.filters=@[blur];[_blurBackdrop addSublayer:_blurShape];
    }else{
      id blur=PIPFilter(@"gaussianBlur");[blur setValue:@6 forKey:@"inputRadius"];[blur setValue:@YES forKey:@"inputNormalizeEdges"];_blurBackdrop.filters=@[blur];
      _blurMask=[CALayer layer];[_blurMask addSublayer:_blurShape];_blurBackdrop.mask=_blurMask;
    }
    id luminance=PIPFilter(@"luminanceCurveMap"),alpha=PIPFilter(@"luminanceToAlpha");
    [luminance setValue:@[@0,@(-1.75),@.09,@2.4] forKey:@"inputValues"];[luminance setValue:@1 forKey:@"inputAmount"];[alpha setValue:@NO forKey:@"inputPremultipliedValues"];
    _shadowBackdrop.filters=@[luminance,alpha];_shadowBackdrop.opacity=.12;
    _shadowMask=[CALayer layer];_shadowMask.masksToBounds=NO;_shadowShape=[CALayer layer];_shadowShape.backgroundColor=NSColor.blackColor.CGColor;
    id shadowBlur=PIPFilter(@"gaussianBlur");[shadowBlur setValue:@14 forKey:@"inputRadius"];_shadowShape.filters=@[shadowBlur];[_shadowMask addSublayer:_shadowShape];_shadowBackdrop.mask=_shadowMask;
    _hideButton=[CALayer layer];_placeButton=[CALayer layer];_hideButton.name=@"hide";_placeButton.name=@"placement";_hideButton.zPosition=100000;_placeButton.zPosition=100000;
    _hideIcon=[CALayer layer];_placeIcon=[CALayer layer];_hideText=[CATextLayer layer];_hideText.alignmentMode=kCAAlignmentCenter;_hideText.truncationMode=kCATruncationNone;
    [_hideButton addSublayer:_hideIcon];[_hideButton addSublayer:_hideText];[_placeButton addSublayer:_placeIcon];
    _hideColor=PIPControlColor(@"controlColor",_white);_placeColor=PIPControlColor(@"controlColor",_white);if(!_hideColor||!_placeColor)return PIPFail(error,@"Native PiP control color filters are unavailable.");
    _hideIcon.filters=@[_hideColor];_placeIcon.filters=@[_placeColor];
    [_controlView.layer addSublayer:_fog];[_controlView.layer addSublayer:_hideButton];[_controlView.layer addSublayer:_placeButton];
    _hideButton.hidden=YES;_placeButton.hidden=YES;
    PIPHost *host=_host;_host=nil;[self attachHost:host?:[self defaultHost]];
    return YES;
  }@catch(NSException *exception){
    [_view stopMonitoring];[_controlView stopMonitoring];[_panel close];[_controlPanel close];_view=nil;_controlView=nil;_panel=nil;_controlPanel=nil;
    return PIPFail(error,exception.reason?:@"Native PiP presentation layers are unavailable.");
  }
}
- (void)setReveal:(double)value animated:(BOOL)animated {
  value=std::clamp(value,0.,1.);if(std::abs(_reveal-value)<.001)return;
  double from=_fog.presentationLayer?_fog.presentationLayer.opacity:_fog.opacity;_reveal=value;
  [CATransaction begin];[CATransaction setDisableActions:YES];
  for(CALayer *layer in @[_fog,_hideButton,_placeButton]){[layer removeAnimationForKey:@"reveal"];layer.opacity=value;if(animated)[layer addAnimation:PIPBasic(@"opacity",@(from),@(value),.12,kCAMediaTimingFunctionEaseInEaseOut) forKey:@"reveal"];}
  [CATransaction commit];_contrastSampling=_controlsVisible&&value>.001;
  if(_contrastSampling)[self startDisplayLink];
}
- (void)updateControls:(CGRect)lead visible:(BOOL)visible above:(BOOL)above docked:(BOOL)docked {
  _controlsVisible=visible;
  if(!visible){_fog.hidden=YES;_hideButton.hidden=YES;_placeButton.hidden=YES;_hideFrame=CGRectZero;_placeFrame=CGRectZero;_fogFrame=CGRectZero;_controlView.tooltips=@[];_contrastSampling=NO;[self setReveal:0 animated:NO];return;}
  NSInteger placement=_host.scope==OCTPIPStackAll?2:(_floating?1:0);NSFont *font=[NSFont systemFontOfSize:12 weight:NSFontWeightSemibold];
  CGFloat hideWidth=placement==0?std::max(22.,std::ceil([_text.hide sizeWithAttributes:@{NSFontAttributeName:font}].width)+16):22;
  CGFloat x=above?CGRectGetMinX(lead)-(placement==0?8:4):CGRectGetMinX(lead)+6,y=above?CGRectGetMaxY(lead)+2:CGRectGetMaxY(lead)-28;
  if(docked&&(_target?_target.alignment:_preferred)==OCTPIPStackTopLeft&&_host.scope==OCTPIPStackAll)x=CGRectGetMaxX(_host.screenRect)-_panel.frame.origin.x;
  BOOL showPlacement=placement!=2&&[self canRequestAccessory];
  _hideFrame=CGRectMake(x,y,hideWidth,22);_placeFrame=showPlacement?CGRectMake(CGRectGetMaxX(_hideFrame)+1,y,22,22):CGRectZero;
  _hideButton.frame=_hideFrame;_placeButton.frame=_placeFrame;_hideButton.hidden=NO;_placeButton.hidden=!showPlacement;_hideIcon.hidden=placement==0;_hideText.hidden=placement!=0;
  CGFloat scale=_controlPanel.backingScaleFactor;
  if(scale>0&&scale!=_iconScale){
    NSImage *symbol=[NSImage imageWithSystemSymbolName:@"xmark" accessibilityDescription:nil];
    NSImageSymbolConfiguration *configuration=[NSImageSymbolConfiguration configurationWithPointSize:14 weight:NSFontWeightHeavy scale:NSImageSymbolScaleLarge];
    configuration=[configuration configurationByApplyingConfiguration:[NSImageSymbolConfiguration configurationWithHierarchicalColor:NSColor.whiteColor]];symbol=[symbol imageWithSymbolConfiguration:configuration];
    NSImage *hide=PIPControlImage(symbol,14,0,scale),*place=PIPControlImage(_placementImage,16,-1,scale);
    if(hide&&place){_hideIcon.contents=(__bridge id)PIPCGImage(hide);_placeIcon.contents=(__bridge id)PIPCGImage(place);_iconScale=scale;}
  }
  _hideIcon.frame=CGRectMake((hideWidth-22)/2,0,22,22);_placeIcon.frame=CGRectMake(0,0,22,22);_hideIcon.contentsScale=scale;_placeIcon.contentsScale=scale;
  CGFloat height=std::ceil(font.ascender-font.descender+font.leading);_hideText.frame=CGRectMake(0,std::floor((22-height)/2),hideWidth,height);_hideText.contentsScale=scale;
  _hideText.string=[[NSAttributedString alloc] initWithString:_text.hide attributes:@{NSFontAttributeName:font,NSForegroundColorAttributeName:[NSColor colorWithWhite:_white alpha:1]}];
  CGRect controls=_hideFrame;if(!CGRectIsEmpty(_placeFrame))controls=CGRectUnion(controls,_placeFrame);
  _fogFrame=CGRectInset(controls,-18,-10);CGRect envelope=CGRectInset(_fogFrame,-22,-22);_fog.frame=envelope;
  CGRect local=(CGRect){CGPointZero,envelope.size},shape=CGRectOffset(_fogFrame,-envelope.origin.x,-envelope.origin.y);
  _blurBackdrop.frame=local;_shadowBackdrop.frame=local;_blurMask.frame=local;_shadowMask.frame=local;_blurShape.frame=shape;_shadowShape.frame=shape;_blurShape.cornerRadius=shape.size.height/2;_shadowShape.cornerRadius=shape.size.height/2;_fog.hidden=NO;
  NSMutableArray *tips=[NSMutableArray arrayWithObject:@{@"rect":[NSValue valueWithRect:_hideFrame],@"text":placement==0?_text.hide:_text.closeTooltip}];
  if(showPlacement)[tips addObject:@{@"rect":[NSValue valueWithRect:_placeFrame],@"text":_text.placementTooltip}];_controlView.tooltips=tips;
  CGPoint pointer=[_controlView convertPoint:_controlPanel.mouseLocationOutsideOfEventStream fromView:nil];[self updateHover:pointer];[self setReveal:_reveal animated:NO];
}
- (NSString *)controlAt:(CGPoint)p {
  if(!_controlsVisible||_reveal<=.001)return nil;
  if(!_hideButton.hidden&&CGRectContainsPoint(_hideFrame,p))return @"hide";
  if(!_placeButton.hidden&&CGRectContainsPoint(_placeFrame,p))return @"placement";return nil;
}
- (void)updateHover:(CGPoint)p {
  if(_closed||_resize)return;
  PIPItem *item=[self itemAt:p];OCTPIPStackAlignment alignment;BOOL resize=[self resizeAt:p alignment:&alignment]!=nil;
  BOOL near=item||(_controlsVisible&&CGRectContainsPoint(_fogFrame,p));NSString *control=resize?nil:[self controlAt:p];_hoverControl=control;
  [CATransaction begin];[CATransaction setDisableActions:YES];
  for(PIPItem *other in [self orderedItems])[other setHover:other==item&&!resize];
  _hideIcon.opacity=[control isEqual:@"hide"]?1:.7;_hideText.opacity=_hideIcon.opacity;_placeIcon.opacity=[control isEqual:@"placement"]?1:.7;[CATransaction commit];
  [self setReveal:near?1:0 animated:YES];
}
- (void)hideFromMenu:(NSMenuItem *)sender {
  NSArray *request=sender.representedObject;PIPItem *item=request.count==2?_items[request[0]]:nil;
  if(![self validItem:item])return;BOOL all=[request[1] boolValue];NSMutableOrderedSet *groups=[NSMutableOrderedSet orderedSet];
  if(all){for(PIPItem *other in [self orderedItems])if([self validItem:other])[groups addObject:other.groupID];}else [groups addObject:item.groupID];
  _callbacks.hideGroupsRequested(groups.array,all);
}
- (void)performControl:(NSString *)control {
  PIPItem *item=nil;for(PIPItem *candidate in [self orderedItems])if(candidate.visible&&[self validItem:candidate]){item=candidate;break;}
  if(!item||![self validHost:_host visible:YES])return;
  NSInteger placement=_host.scope==OCTPIPStackAll?2:(_floating?1:0);
  if([control isEqual:@"hide"]&&placement==0){
    NSMenu *menu=[[NSMenu alloc] initWithTitle:@""];
    for(NSNumber *all in @[@NO,@YES]){NSMenuItem *choice=[[NSMenuItem alloc] initWithTitle:all.boolValue?_text.hideForAllGroups:_text.hideForGroup action:@selector(hideFromMenu:) keyEquivalent:@""];choice.target=self;choice.representedObject=@[item.identity,all];[menu addItem:choice];}
    [menu popUpMenuPositioningItem:nil atLocation:CGPointMake(_hideFrame.origin.x,_hideFrame.origin.y) inView:_controlView];return;
  }
  if([control isEqual:@"hide"]){_callbacks.hideGroupsRequested(@[item.groupID],NO);return;}
  if([control isEqual:@"placement"]&&(placement==2||![self canRequestAccessory]))return;
  BOOL accessory=[control isEqual:@"placement"];if(!accessory&&![control isEqual:@"hide"])return;
  _pendingPlacement=item.identity;_pendingRole=accessory?OCTPIPStackAccessory:OCTPIPStackMain;
  [self reconcilePendingHost];
  if(!_pendingPlacement||_closed||![self validItem:item])return;
  BOOL accepted=accessory?(_callbacks.sendToAccessoryRequested&&_callbacks.sendToAccessoryRequested(item.identity)):_callbacks.returnToMainRequested(item.identity);
  if(![self validItem:item])return;
  if(!accepted){_pendingPlacement=nil;_pendingRole=-1;_callbacks.interrupted(item.identity,PIPError(@"The client could not open the requested PiP host."));return;}
  [self reconcilePendingHost];
}
- (void)updateContrast:(double)timestamp force:(BOOL)force {
  if(_closed||_motion||_resize||!_controlsVisible||_reveal<=.001||!_controlPanel.visible||![self validHost:_host visible:YES]||(!force&&timestamp-_lastSample<.05))return;
  _lastSample=timestamp;NSInteger placement=_host.scope==OCTPIPStackAll?2:(_floating?1:0);
  CGRect region=placement==0?_hideFrame:CGRectMake(CGRectGetMidX(_hideFrame)-7,CGRectGetMidY(_hideFrame)-7,14,14);
  if(!_placeButton.hidden)region=CGRectUnion(region,CGRectMake(CGRectGetMidX(_placeFrame)-8,CGRectGetMidY(_placeFrame)-8,16,16));
  region=[_controlPanel convertRectToScreen:[_controlView convertRect:region toView:nil]];
  NSScreen *base=nil;for(NSScreen *screen in NSScreen.screens)if(CGRectContainsPoint(screen.frame,CGPointZero)){base=screen;break;}if(!base||!PIPRect(region))return;
  region.origin.y=CGRectGetMaxY(base.frame)-CGRectGetMaxY(region);
  using Capture=CGImageRef(*)(CGRect,CGWindowListOption,CGWindowID,CGWindowImageOption);
  Capture capture=reinterpret_cast<Capture>(dlsym(RTLD_DEFAULT,"CGWindowListCreateImage"));if(!capture)return;
  CGWindowID window=(CGWindowID)_controlPanel.windowNumber;if(!window)return;
  CGImageRef image=capture(region,kCGWindowListOptionOnScreenBelowWindow,window,kCGWindowImageBoundsIgnoreFraming|kCGWindowImageNominalResolution);if(!image)return;
  unsigned char bytes[24*12*4]={0};CGColorSpaceRef space=CGColorSpaceCreateDeviceRGB();CGContextRef context=CGBitmapContextCreate(bytes,24,12,8,96,space,static_cast<CGBitmapInfo>(kCGImageAlphaPremultipliedLast)|kCGBitmapByteOrder32Big);CGColorSpaceRelease(space);
  if(!context){CGImageRelease(image);return;}CGContextSetInterpolationQuality(context,kCGInterpolationLow);CGContextDrawImage(context,CGRectMake(0,0,24,12),image);CGContextRelease(context);CGImageRelease(image);
  double sum=0,weight=0;for(NSUInteger i=0;i<24*12;++i){double a=bytes[4*i+3]/255.;if(a<=.05)continue;double r=std::min(1.,bytes[4*i]/255./a),g=std::min(1.,bytes[4*i+1]/255./a),b=std::min(1.,bytes[4*i+2]/255./a);sum+=(.2126*r+.7152*g+.0722*b)*a;weight+=a;}
  if(weight<=0)return;double luminance=sum/weight,next=_white;
  if(_white>.5){_lightSamples=0;if(luminance>.74)next=0;}
  else if(luminance<.56){if(timestamp-_lastColorChange>=.24&&++_lightSamples>=3)next=1;}else _lightSamples=0;
  if(next==_white)return;double previous=_white;_white=next;_lastColorChange=timestamp;_lightSamples=0;
  [CATransaction begin];[CATransaction setDisableActions:YES];
  for(CALayer *layer in @[_hideIcon,_placeIcon]){CIFilter *filter=layer==_hideIcon?_hideColor:_placeColor;[filter setValue:[CIVector vectorWithX:next Y:next Z:next W:0] forKey:@"inputBiasVector"];[layer addAnimation:PIPBasic(@"filters.controlColor.inputBiasVector",[CIVector vectorWithX:previous Y:previous Z:previous W:0],[CIVector vectorWithX:next Y:next Z:next W:0],.16,kCAMediaTimingFunctionEaseInEaseOut) forKey:@"contrast"];}
  _hideText.string=[[NSAttributedString alloc] initWithString:_text.hide attributes:@{NSFontAttributeName:[NSFont systemFontOfSize:12 weight:NSFontWeightSemibold],NSForegroundColorAttributeName:[NSColor colorWithWhite:next alpha:1]}];[CATransaction commit];
}

- (PIPItem *)itemAt:(CGPoint)point {
  if(_closed||!PIPPoint(point)||![self validHost:_host visible:YES])return nil;
  PIPItem *hit=nil;CGFloat z=-CGFLOAT_MAX;
  for(PIPItem *item in [self orderedItems])if(item.visible&&item.contributes&&[self validItem:item]&&item.root.zPosition>=z&&CGRectContainsPoint(item.frame,point)){hit=item;z=item.root.zPosition;}
  return hit;
}
- (PIPItem *)resizeAt:(CGPoint)point alignment:(OCTPIPStackAlignment *)alignment {
  if(_closed||!PIPPoint(point)||![self validHost:_host visible:YES])return nil;
  PIPItem *hit=nil;CGFloat z=-CGFLOAT_MAX;OCTPIPStackAlignment selected=OCTPIPStackTopLeft;
  for(PIPItem *item in [self orderedItems]){
    if(!item.visible||!item.contributes||![self validItem:item]||item.root.zPosition<z)continue;
    CGRect f=item.frame;
    CGRect corners[]={CGRectMake(CGRectGetMaxX(f)-14,f.origin.y,14,14),CGRectMake(f.origin.x,f.origin.y,14,14),CGRectMake(f.origin.x,CGRectGetMaxY(f)-14,14,14),CGRectMake(CGRectGetMaxX(f)-14,CGRectGetMaxY(f)-14,14,14)};
    for(NSInteger i=0;i<4;++i)if(CGRectContainsPoint(corners[i],point)){hit=item;z=item.root.zPosition;selected=(OCTPIPStackAlignment)i;}
  }
  if(hit&&alignment)*alignment=selected;
  return hit;
}
- (CGRect)passthroughScreenRect { return [self validHost:_host visible:YES]?_host.passthroughScreenRect:CGRectZero; }
- (BOOL)acceptsPoint:(CGPoint)point controlSurface:(BOOL)control {
  if(_closed||!PIPPoint(point)||![self validHost:_host visible:YES])return NO;
  NSWindow *window=control?_controlPanel:_panel;NSView *view=control?_controlView:_view;
  if(!window.visible||!view)return NO;
  CGPoint screen=[window convertPointToScreen:[view convertPoint:point toView:nil]];
  if(CGRectContainsPoint([self passthroughScreenRect],screen))return NO;
  return [self resizeAt:point alignment:nullptr]||[self itemAt:point]||[self controlAt:point];
}
- (NSCursor *)resizeCursor:(OCTPIPStackAlignment)alignment {
  if(alignment<0||alignment>3)return nil;
  BOOL ascending=alignment==0||alignment==2;
  if(@available(macOS 15.0,*))return [NSCursor frameResizeCursorFromPosition:ascending?NSCursorFrameResizePositionBottomRight:NSCursorFrameResizePositionBottomLeft inDirections:NSCursorFrameResizeDirectionsAll];
  NSString *name=ascending?@"arrow.up.left.and.arrow.down.right":@"arrow.down.left.and.arrow.up.right";
  NSImage *symbol=[NSImage imageWithSystemSymbolName:name accessibilityDescription:nil];
  if(!symbol)return nil;
  NSImage *configured=[symbol imageWithSymbolConfiguration:[NSImageSymbolConfiguration configurationWithPointSize:17 weight:NSFontWeightSemibold]];
  if(configured)symbol=configured;
  NSImage *image=[[NSImage alloc] initWithSize:NSMakeSize(24,24)];
  [image lockFocus];[[NSColor.blackColor colorWithAlphaComponent:.92] set];
  [symbol drawInRect:CGRectMake((24-symbol.size.width)/2,(24-symbol.size.height)/2,symbol.size.width,symbol.size.height) fromRect:NSZeroRect operation:NSCompositingOperationSourceOver fraction:1 respectFlipped:NO hints:nil];
  [image unlockFocus];return [[NSCursor alloc] initWithImage:image hotSpot:CGPointMake(12,12)];
}
- (void)refreshCursorRects { [_panel invalidateCursorRectsForView:_view];[_controlPanel invalidateCursorRectsForView:_controlView];[_view refreshPointer];[_controlView refreshPointer]; }
- (BOOL)beginResize:(CGPoint)point screen:(CGPoint)screen {
  if(!PIPPoint(screen))return NO;
  OCTPIPStackAlignment alignment;PIPItem *item=[self resizeAt:point alignment:&alignment];
  if(!item||![self validItem:item])return NO;
  _drag=nil;_move=nil;_motion=NO;_hostFollow=NO;_hoverControl=nil;_hasFrozenEnvelope=NO;_expandedSize=CGSizeZero;
  [self stopDisplayLink];[self updateOffsets];
  if(_closed||![self validItem:item])return NO;
  PIPResize *resize=[PIPResize new];resize.leader=item;resize.alignment=alignment;resize.start=screen;
  resize.initialMax=std::max(item.displaySize.width,item.displaySize.height);resize.previousScaling=item.scalesContent;
  resize.fixedPoint=[self snapPoint:PIPAdd(item.origin,PIPFixed(item.displaySize,alignment))];
  CGPoint anchor=[self currentAnchor];_target=[PIPAnchor at:anchor alignment:_target?_target.alignment:_preferred host:_host];
  _resize=resize;[item setHover:YES];[self applyFrames:YES];return !_closed&&_resize==resize;
}
- (void)resizeTo:(CGPoint)screen {
  PIPResize *resize=_resize;if(!resize||!PIPPoint(screen))return;
  if(![self validHost:_host visible:YES]||![self validItem:resize.leader]){[self interrupt:resize.leader error:PIPError(@"PiP resize source or host is no longer available.")];return;}
  CGFloat next=std::clamp(resize.initialMax+(screen.y-resize.start.y)*(PIPUp(resize.alignment)?1:-1),100.,400.);
  if(std::abs(next-_maxSize)<.5)return;
  _maxSize=next;_customMax=YES;
  for(PIPItem *item in [self motionItems]){item.maxSize=next;item.displaySize=PIPFit(item.sourceSize,next);item.scalesContent=YES;}
  [self updateOffsets];if(_closed||_resize!=resize||![self validItem:resize.leader])return;
  CGPoint anchor=PIPSub(PIPSub(resize.fixedPoint,PIPFixed(resize.leader.displaySize,resize.alignment)),resize.leader.restOffset);
  for(PIPItem *item in [self motionItems]){item.origin=[self snapPoint:PIPAdd(anchor,item.restOffset)];item.target=item.origin;item.velocity=CGPointZero;}
  [self applyFrames:YES];
}
- (void)endResize {
  PIPResize *resize=_resize;if(!resize)return;
  _resize=nil;_hasFrozenEnvelope=NO;_frozenEnvelope=CGRectZero;
  if(_closed||![self validItem:resize.leader])return;
  BOOL changed=std::abs(_maxSize-resize.initialMax)>=.5;
  if(!changed&&!resize.previousScaling)for(PIPItem *item in [self motionItems])item.scalesContent=NO;
  [self updateOffsets];[self configureSprings:resize.leader dragging:NO programmatic:NO];
  if(_closed)return;
  [self startMotion];
  if(changed)_callbacks.maxDisplaySizeRequested(_maxSize);
  if(_closed)return;
  if(_pendingLayout){BOOL animated=_pendingAnimated;_pendingLayout=NO;_pendingAnimated=NO;[self layoutAnimated:animated];}
}
- (BOOL)beginDrag:(CGPoint)point {
  if(_resize)return NO;
  PIPItem *item=[self itemAt:point];if(!item)return NO;
  _move=nil;_hostFollow=NO;_hoverControl=nil;[self updateOffsets];
  if(_closed||![self validItem:item])return NO;
  CGPoint screen=[_panel convertPointToScreen:[_view convertPoint:point toView:nil]];
  PIPDrag *drag=[PIPDrag new];drag.leader=item;drag.start=screen;drag.pointer=screen;
  drag.grab=PIPSub(screen,item.origin);drag.timestamp=CACurrentMediaTime();drag.velocity=CGPointZero;
  _drag=drag;[item setHover:YES];[self configureSprings:item dragging:YES programmatic:NO];[self startMotion];return !_closed&&_drag==drag;
}
- (void)dragTo:(CGPoint)point {
  PIPDrag *drag=_drag;if(!drag||!PIPPoint(point))return;
  if(![self validItem:drag.leader]||![self validHost:_host visible:YES]){[self interrupt:drag.leader error:PIPError(@"PiP drag source or host is no longer available.")];return;}
  [self updateHover:point];
  CGPoint screen=[_panel convertPointToScreen:[_view convertPoint:point toView:nil]];double now=CACurrentMediaTime();
  drag.velocity=PIPAdd(PIPMul(drag.velocity,.72),PIPMul(PIPSub(screen,drag.pointer),.28/std::max(now-drag.timestamp,1./240)));
  drag.moved=drag.moved||PIPLength(PIPSub(screen,drag.start))>=4;drag.pointer=screen;drag.timestamp=now;
  [self startMotion];
}
- (void)endDrag {
  PIPDrag *drag=_drag;if(!drag)return;
  if(![self validItem:drag.leader]){_drag=nil;return;}
  if(!drag.moved&&[[_order firstObject] isEqual:drag.leader.identity])_callbacks.focusRequested(drag.leader.identity);
  if(_closed||_drag!=drag||![self validItem:drag.leader])return;
  CGPoint current=[self currentAnchor],velocity=PIPMul(drag.velocity,.55);double speed=PIPLength(velocity);
  CGPoint projected=PIPAdd(current,PIPMul(velocity,std::clamp(.18+speed/5000,.18,.45)));
  NSMutableArray<PIPHost *> *hosts=[NSMutableArray arrayWithObject:_host];
  if(drag.moved||PIPLength(drag.velocity)>=120){
    PIPHost *main=[self defaultHost];if(main&&main!=_host)[hosts addObject:main];
    for(PIPHost *host in _hosts.allValues)if(![hosts containsObject:host]&&[self validHost:host visible:YES])[hosts addObject:host];
  }
  PIPAnchor *best=nil;double score=CGFLOAT_MAX;
  for(PIPHost *host in hosts)for(PIPAnchor *anchor in [self anchorsForHost:host]){
    double penalty=0;
    if(host!=_host){if(!CGRectContainsPoint(CGRectInset(host.screenRect,-120,-120),projected))continue;penalty=180;}
    CGPoint delta=PIPSub(anchor.point,current);double distance=PIPLength(delta),bias=0;
    if(speed>0&&distance>0)bias=std::max(0.,(velocity.x*delta.x+velocity.y*delta.y)/(speed*distance))*speed*.12;
    double candidate=PIPLength(PIPSub(anchor.point,projected))-bias+penalty;
    if(candidate<score){score=candidate;best=anchor;}
  }
  _drag=nil;[_order removeObject:drag.leader.identity];[_order insertObject:drag.leader.identity atIndex:0];
  [self updateOffsets];[self configureSprings:drag.leader dragging:NO programmatic:NO];
  if(best){[self attachHost:best.host];_target=best;_preferred=best.alignment;for(PIPItem *item in [self motionItems])item.velocity=drag.velocity;[self startMotion];}
  else [self layoutAnimated:YES];
}
- (void)interrupt:(PIPItem *)item error:(NSError *)error {
  if(_closed)return;
  if(item){[self invalidateIdentity:item.identity error:error];return;}
  _drag=nil;_resize=nil;_move=nil;_motion=NO;_contrastSampling=NO;_hasFrozenEnvelope=NO;
  [self stopDisplayLink];[_panel orderOut:nil];[_controlPanel orderOut:nil];
  _callbacks.interrupted(nil,error);
}
- (void)invalidateIdentity:(OCTPIPStackIdentity *)identity error:(NSError *)error {
  PIPMain();if(_closed||!identity||(!_items[identity]&&!_exits[identity]))return;
  [self removeItem:identity animated:NO completion:nil];
  if(!_closed)_callbacks.interrupted(identity,error);
}
- (void)close {
  PIPMain();if(_closed)return;_closed=YES;
  [self stopDisplayLink];_drag=nil;_resize=nil;_move=nil;_target=nil;_pendingPlacement=nil;
  [_view stopMonitoring];[_controlView stopMonitoring];_view.stack=nil;_controlView.stack=nil;
  for(id observer in _observers)[NSNotificationCenter.defaultCenter removeObserver:observer];[_observers removeAllObjects];
  NSMutableArray *items=[_items.allValues mutableCopy];[items addObjectsFromArray:_exits.allValues];[_items removeAllObjects];[_exits removeAllObjects];[_order removeAllObjects];
  NSMutableArray *completions=[NSMutableArray array];
  for(PIPItem *item in items){[item.embeddedView removeFromSuperview];item.embeddedView=nil;item.embeddedHost=nil;if(item.removalCompletion)[completions addObject:item.removalCompletion];item.removalCompletion=nil;[item detach];}
  [_panel.parentWindow removeChildWindow:_panel];[_controlPanel.parentWindow removeChildWindow:_controlPanel];
  [_panel close];[_controlPanel close];_panel=nil;_controlPanel=nil;_view=nil;_controlView=nil;_host=nil;
  [_hosts removeAllObjects];[_preferences removeAllObjects];_callbacks=nil;
  for(void(^completion)(void) in completions)completion();
}
- (void)dealloc { [self close]; }
@end

@implementation PIPView
- (instancetype)initWithFrame:(NSRect)frame {
  if((self=[super initWithFrame:frame])){self.wantsLayer=YES;self.layer.backgroundColor=NSColor.clearColor.CGColor;_tooltips=@[];}
  return self;
}
- (BOOL)isOpaque { return NO; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (void)stopMonitoring {
  if(_localMonitor){[NSEvent removeMonitor:_localMonitor];_localMonitor=nil;}
  if(_globalMonitor){[NSEvent removeMonitor:_globalMonitor];_globalMonitor=nil;}
  [self removeAllToolTips];_pressedControl=nil;_activeCursor=nil;_resizing=NO;_dragging=NO;
}
- (void)viewDidMoveToWindow {
  [super viewDidMoveToWindow];[self stopMonitoring];
  if(!self.window)return;
  __weak PIPView *weak=self;
  _globalMonitor=[NSEvent addGlobalMonitorForEventsMatchingMask:NSEventMaskMouseMoved handler:^(NSEvent *event){[weak refreshPointer];}];
  _localMonitor=[NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskMouseMoved handler:^NSEvent *(NSEvent *event){[weak refreshPointer];return event;}];
  [self.window invalidateCursorRectsForView:self];[self refreshPointer];
}
- (void)updateTrackingAreas {
  if(_area)[self removeTrackingArea:_area];
  _area=[[NSTrackingArea alloc] initWithRect:NSZeroRect options:NSTrackingMouseEnteredAndExited|NSTrackingMouseMoved|NSTrackingCursorUpdate|NSTrackingActiveAlways|NSTrackingInVisibleRect|NSTrackingEnabledDuringMouseDrag owner:self userInfo:nil];
  [self addTrackingArea:_area];[super updateTrackingAreas];
}
- (void)setTooltips:(NSArray<NSDictionary *> *)tooltips {
  _tooltips=[tooltips copy];[self removeAllToolTips];
  for(NSDictionary *entry in _tooltips){CGRect rect=[entry[@"rect"] rectValue];if(PIPRect(rect)&&[entry[@"text"] isKindOfClass:NSString.class])[self addToolTipRect:rect owner:self userData:nullptr];}
}
- (NSString *)view:(NSView *)view stringForToolTip:(NSToolTipTag)tag point:(NSPoint)point userData:(void *)data {
  for(NSDictionary *entry in _tooltips)if(CGRectContainsPoint([entry[@"rect"] rectValue],point))return entry[@"text"];
  return @"";
}
- (NSView *)hitTest:(NSPoint)point { return [_stack acceptsPoint:point controlSurface:_controlSurface]?self:nil; }
- (void)refreshPointer {
  OCTPIPStack *stack=_stack;NSWindow *window=self.window;if(!stack||!window.visible)return;
  CGPoint point=[self convertPoint:window.mouseLocationOutsideOfEventStream fromView:nil];[stack updateHover:point];
  if(_resizing){[_activeCursor set];return;}
  OCTPIPStackAlignment alignment;NSCursor *cursor=[stack resizeAt:point alignment:&alignment]?[stack resizeCursor:alignment]:nil;
  if(cursor)[cursor set];else if([stack acceptsPoint:point controlSurface:_controlSurface])[NSCursor.arrowCursor set];
}
- (void)mouseEntered:(NSEvent *)event { [self refreshPointer]; }
- (void)mouseMoved:(NSEvent *)event { [self refreshPointer]; }
- (void)mouseExited:(NSEvent *)event { if(!_resizing)[NSCursor.arrowCursor set];[self refreshPointer]; }
- (void)cursorUpdate:(NSEvent *)event { [self refreshPointer]; }
- (void)resetCursorRects {
  [super resetCursorRects];OCTPIPStack *stack=_stack;if(!stack)return;
  for(PIPItem *item in [stack orderedItems]){
    if(!item.visible||!item.contributes||![stack validItem:item])continue;
    CGRect f=item.frame;CGRect corners[]={CGRectMake(CGRectGetMaxX(f)-14,f.origin.y,14,14),CGRectMake(f.origin.x,f.origin.y,14,14),CGRectMake(f.origin.x,CGRectGetMaxY(f)-14,14,14),CGRectMake(CGRectGetMaxX(f)-14,CGRectGetMaxY(f)-14,14,14)};
    for(NSInteger i=0;i<4;++i){NSCursor *cursor=[stack resizeCursor:(OCTPIPStackAlignment)i];if(cursor)[self addCursorRect:corners[i] cursor:cursor];}
  }
}
- (void)mouseDown:(NSEvent *)event {
  OCTPIPStack *stack=_stack;if(!stack)return;
  CGPoint point=[self convertPoint:event.locationInWindow fromView:nil];if(![stack acceptsPoint:point controlSurface:_controlSurface])return;
  [stack updateHover:point];OCTPIPStackAlignment alignment;BOOL handle=[stack resizeAt:point alignment:&alignment]!=nil;
  if([stack beginResize:point screen:[self.window convertPointToScreen:event.locationInWindow]]){_resizing=YES;_activeCursor=handle?[stack resizeCursor:alignment]:nil;[(_activeCursor?:NSCursor.arrowCursor) set];return;}
  _pressedControl=[[stack controlAt:point] copy];_down=point;
  if(!_pressedControl)_dragging=[stack beginDrag:point];
}
- (void)mouseDragged:(NSEvent *)event {
  OCTPIPStack *stack=_stack;if(!stack)return;
  if(_resizing){[stack resizeTo:[self.window convertPointToScreen:event.locationInWindow]];[_activeCursor set];return;}
  CGPoint point=[self convertPoint:[self.window convertPointFromScreen:NSEvent.mouseLocation] fromView:nil];
  if(_pressedControl){if(PIPLength(PIPSub(point,_down))<4)return;_pressedControl=nil;_dragging=[stack beginDrag:_down];}
  if(_dragging)[stack dragTo:point];
}
- (void)mouseUp:(NSEvent *)event {
  OCTPIPStack *stack=_stack;CGPoint point=[self convertPoint:event.locationInWindow fromView:nil];
  if(_resizing){[stack resizeTo:[self.window convertPointToScreen:event.locationInWindow]];[stack endResize];_resizing=NO;_activeCursor=nil;[self.window invalidateCursorRectsForView:self];}
  else if(_pressedControl){NSString *control=_pressedControl;_pressedControl=nil;if([control isEqual:[stack controlAt:point]])[stack performControl:control];}
  else if(_dragging){_dragging=NO;[stack endDrag];}
  [self refreshPointer];
}
- (void)dealloc { [self stopMonitoring]; }
@end
