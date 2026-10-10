#import "permissions.h"
#import "OctopusSettingsVisuals-Swift.h"
#include <cmath>
#include <limits>
#include <libproc.h>
#include <unistd.h>
#include <cstring>
#include <atomic>
#include <memory>
#import <QuartzCore/QuartzCore.h>

namespace {
BOOL Fail(NSError **error, NSString *message) {
  if(error)*error=[NSError errorWithDomain:@"OctopusNativePermissions" code:1 userInfo:@{NSLocalizedDescriptionKey:message}];
  return NO;
}
BOOL Text(id value) { return [value isKindOfClass:NSString.class] && [value length]>0 && [value length]<=4096 && [value rangeOfString:[NSString stringWithFormat:@"%C",(unichar)0]].location==NSNotFound; }
BOOL Keys(id value, NSArray<NSString*> *keys) { return [value isKindOfClass:NSDictionary.class] && [value count]==keys.count && [[NSSet setWithArray:[value allKeys]] isEqualToSet:[NSSet setWithArray:keys]]; }
BOOL ProcessID(id value) {
  if(![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value)==CFBooleanGetTypeID())return NO;
  double number=[value doubleValue];return std::isfinite(number) && number>0 && number<=INT32_MAX && std::floor(number)==number;
}
BOOL Bundle(id value) { return value==NSNull.null || (Keys(value,@[@"identifier",@"path"]) && Text(value[@"identifier"]) && Text(value[@"path"]) && [value[@"path"] isAbsolutePath]); }
BOOL Flag(id value) { return [value isKindOfClass:NSNumber.class] && CFGetTypeID((__bridge CFTypeRef)value)==CFBooleanGetTypeID(); }
BOOL NativeProcess(id value) {
  if(!Keys(value,@[@"process_id",@"executable_path",@"display_name",@"bundle",@"responsible_app",@"start_seconds",@"start_microseconds"]) || !ProcessID(value[@"process_id"])
    || !Text(value[@"executable_path"]) || ![value[@"executable_path"] isAbsolutePath] || !Text(value[@"display_name"]) || !Bundle(value[@"bundle"]) || !Bundle(value[@"responsible_app"]))return NO;
  for(NSString *key in @[@"start_seconds",@"start_microseconds"])if(!Text(value[key]) || [value[key] length]>20 || [value[key] rangeOfCharacterFromSet:NSCharacterSet.decimalDigitCharacterSet.invertedSet].location!=NSNotFound)return NO;
  return YES;
}
BOOL Snapshot(NSDictionary *value, NSError **error) {
  if(!Keys(value,@[@"schema_version",@"host_generation",@"helper",@"permissions",@"automation"]) || ![value[@"schema_version"] isEqual:@1] || !Text(value[@"host_generation"]))return Fail(error,@"The permission status identity is invalid.");
  NSDictionary *helper=value[@"helper"]; NSArray *rows=value[@"permissions"];
  if(!Keys(helper,@[@"executable_path",@"display_name",@"process_id",@"bundle",@"responsible_app"]) || !Text(helper[@"executable_path"]) || ![helper[@"executable_path"] isAbsolutePath] || !Bundle(helper[@"bundle"]) || !Bundle(helper[@"responsible_app"])
      || !Text(helper[@"display_name"]) || !ProcessID(helper[@"process_id"])
      || ![rows isKindOfClass:NSArray.class] || rows.count>64)return Fail(error,@"The permission helper or requirement list is invalid.");
  NSSet *kinds=[NSSet setWithArray:@[@"screen_capture",@"accessibility"]];
  NSSet *statuses=[NSSet setWithArray:@[@"granted",@"not_granted",@"not_determined",@"denied",@"restricted",@"unavailable"]];
  NSMutableSet *ids=[NSMutableSet set];
  for(NSDictionary *row in rows){
    if(![row isKindOfClass:NSDictionary.class] || !Text(row[@"id"]) || [ids containsObject:row[@"id"]] || ![kinds containsObject:row[@"kind"]]
        || !Text(row[@"title"]) || ![statuses containsObject:row[@"status"]] || !Flag(row[@"can_request"]) || !Flag(row[@"can_open_settings"])
        || (row[@"detail"] && row[@"detail"]!=NSNull.null && !Text(row[@"detail"]))
        || !Keys(row[@"provenance"],@[@"process_id",@"api"]) || !ProcessID(row[@"provenance"][@"process_id"]) || ![row[@"provenance"][@"process_id"] isEqual:helper[@"process_id"]]
        || ![row[@"provenance"][@"api"] isEqual:([row[@"kind"] isEqual:@"screen_capture"]?@"CGPreflightScreenCaptureAccess":@"AXIsProcessTrusted")])return Fail(error,@"A native permission requirement is invalid.");
    [ids addObject:row[@"id"]];
  }
  NSArray *automation=value[@"automation"];
  if(![automation isKindOfClass:NSArray.class]||automation.count+rows.count>64)return Fail(error,@"The native Automation list is invalid.");
  for(NSDictionary *row in automation){
    if(![row isKindOfClass:NSDictionary.class])return Fail(error,@"The native Automation row is invalid.");
    NSDictionary *origin=row[@"provenance"];
    if(!Keys(row,@[@"id",@"kind",@"title",@"status",@"can_request",@"can_open_settings",@"detail",@"provenance"]) || !Text(row[@"id"]) || [ids containsObject:row[@"id"]]
      || ![row[@"kind"] isEqual:@"automation"] || !Text(row[@"title"]) || ![statuses containsObject:row[@"status"]] || !Flag(row[@"can_request"]) || !Flag(row[@"can_open_settings"]) || !Text(row[@"detail"])
      || !Keys(origin,@[@"api",@"event_class",@"event_id",@"sender",@"recipient"]) || ![origin[@"api"] isEqual:@"AEDeterminePermissionToAutomateTarget"] || ![origin[@"event_class"] isEqual:@"OcPi"] || ![origin[@"event_id"] isEqual:@"BtV1"]
      || !NativeProcess(origin[@"sender"]) || !NativeProcess(origin[@"recipient"]) || [origin[@"sender"][@"process_id"] intValue]!=getpid()
      || ![origin[@"recipient"][@"process_id"] isEqual:helper[@"process_id"]] || ![origin[@"recipient"][@"executable_path"] isEqual:helper[@"executable_path"]])return Fail(error,@"The native Automation sender or recipient is invalid.");
    [ids addObject:row[@"id"]];
  }
  return YES;
}
NSTextField *Label(NSString *text, NSFont *font, NSColor *color) {
  NSTextField *value=[NSTextField wrappingLabelWithString:text];value.font=font;value.textColor=color;
  value.selectable=YES;value.maximumNumberOfLines=0;return value;
}
}
namespace {
NSDictionary *ProcessIdentity(pid_t pid, NSError **error);
BOOL SameProcess(NSDictionary *expected);
NSRect SettingsRect(NSDictionary *identity, NSNumber *windowID) {
  if(!SameProcess(identity))return NSZeroRect;
  NSArray *space=[NSWindow windowNumbersWithOptions:NSWindowNumberListAllApplications];
  if(![space containsObject:windowID])return NSZeroRect;
  NSArray *windows=CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly|kCGWindowListExcludeDesktopElements,kCGNullWindowID));
  for(NSDictionary *window in windows){
    if(![window[(__bridge NSString *)kCGWindowNumber] isEqual:windowID])continue;
    CGRect rect;id bounds=window[(__bridge NSString *)kCGWindowBounds];if(![bounds isKindOfClass:NSDictionary.class])return NSZeroRect;
    if([window[(__bridge NSString *)kCGWindowOwnerPID] intValue]!=[identity[@"process_id"] intValue]
      || !CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)window[(__bridge NSString *)kCGWindowBounds],&rect)
      || !std::isfinite(rect.origin.x)||!std::isfinite(rect.origin.y)||!std::isfinite(rect.size.width)||!std::isfinite(rect.size.height)
      || rect.size.width<=0||rect.size.height<=0)return NSZeroRect;
    return NSRectFromCGRect(rect);
  }
  return NSZeroRect;
}
}
@interface OCTSettingsWindow : NSPanel
@end
@implementation OCTSettingsWindow
- (BOOL)canBecomeKeyWindow { return NO; }
- (BOOL)canBecomeMainWindow { return NO; }
@end
@interface OCTSettingsDragView : NSView <NSDraggingSource,NSPasteboardItemDataProvider>
@property(nonatomic,strong) NSView *applicationRow;
@property(nonatomic,copy,nullable) NSURL *(^currentURL)(void);
@property(nonatomic,copy,nullable) void (^dropped)(NSPoint);
@end
@implementation OCTSettingsDragView
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (void)mouseDown:(NSEvent *)event {
  NSURL *url=self.currentURL?self.currentURL():nil;if(!url)return;
  NSBitmapImageRep *bitmap=[self.applicationRow bitmapImageRepForCachingDisplayInRect:self.applicationRow.bounds];if(!bitmap)return;
  [self.applicationRow cacheDisplayInRect:self.applicationRow.bounds toBitmapImageRep:bitmap];
  NSImage *image=[[NSImage alloc] initWithSize:self.applicationRow.bounds.size];[image addRepresentation:bitmap];
  NSPasteboardItem *payload=[NSPasteboardItem new];[payload setDataProvider:self forTypes:@[NSPasteboardTypeFileURL]];
  NSDraggingItem *item=[[NSDraggingItem alloc] initWithPasteboardWriter:payload];
  [item setDraggingFrame:[self convertRect:self.applicationRow.bounds fromView:self.applicationRow] contents:image];
  NSDraggingSession *session=[self beginDraggingSessionWithItems:@[item] event:event source:self];session.animatesToStartingPositionsOnCancelOrFail=YES;
}
- (NSDragOperation)draggingSession:(NSDraggingSession *)session sourceOperationMaskForDraggingContext:(NSDraggingContext)context { return context==NSDraggingContextOutsideApplication?NSDragOperationCopy:NSDragOperationNone; }
- (void)draggingSession:(NSDraggingSession *)session willBeginAtPoint:(NSPoint)point { self.applicationRow.hidden=YES; }
- (void)draggingSession:(NSDraggingSession *)session endedAtPoint:(NSPoint)point operation:(NSDragOperation)operation {
  self.applicationRow.hidden=NO;if(operation==NSDragOperationCopy&&self.dropped)self.dropped(point);
}
- (void)pasteboard:(NSPasteboard *)pasteboard item:(NSPasteboardItem *)item provideDataForType:(NSPasteboardType)type {
  NSURL *url=self.currentURL?self.currentURL():nil;
  if(url&&[type isEqual:NSPasteboardTypeFileURL])[item setData:url.dataRepresentation forType:type];
}
@end
@interface OCTSettingsAccessory : NSObject
- (nullable instancetype)initWithHelper:(NSDictionary *)helper permission:(NSDictionary *)permission resourceBundle:(NSBundle *)bundle error:(NSError **)error;
- (BOOL)update:(NSDictionary *)source error:(NSError **)error;
- (void)hide;
- (void)invalidate;
@end
@implementation OCTSettingsAccessory {
  OCTSettingsWindow *_window,*_arrowWindow;
  OCTSettingsDragView *_drag;
  OCTSettingsVisuals *_visuals;
  NSDictionary *_helper,*_helperIncarnation,*_source;
  NSRunningApplication *_application;
  NSTimer *_timer;
  BOOL _invalidated;
}
- (instancetype)initWithHelper:(NSDictionary *)helper permission:(NSDictionary *)permission resourceBundle:(NSBundle *)bundle error:(NSError **)error {
  NSImage *arrow=[bundle imageForResource:@"HintArrow"];
  if(!arrow||arrow.size.height<=0||arrow.size.width<=0){Fail(error,@"The exact packaged Settings arrow resource is unavailable.");return nil;}
  if(!(self=[super init]))return nil;_helper=[helper copy];
  _helperIncarnation=ProcessIdentity([helper[@"process_id"] intValue],error);
  if(!_helperIncarnation||![_helperIncarnation[@"executable_path"] isEqual:helper[@"executable_path"]]||![_helperIncarnation[@"bundle"] isEqual:helper[@"bundle"]]){Fail(error,@"The requesting native permission process changed before presentation.");return nil;}
  _window=[[OCTSettingsWindow alloc] initWithContentRect:NSMakeRect(0,0,531,110)
    styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskUtilityWindow|NSWindowStyleMaskNonactivatingPanel|NSWindowStyleMaskFullSizeContentView backing:NSBackingStoreBuffered defer:NO];
  _window.releasedWhenClosed=NO;_window.hasShadow=YES;_window.level=NSFloatingWindowLevel;_window.movable=NO;_window.movableByWindowBackground=NO;
  _window.titleVisibility=NSWindowTitleHidden;_window.titlebarAppearsTransparent=YES;_window.toolbarStyle=NSWindowToolbarStyleUnified;
  _window.backgroundColor=[NSColor.whiteColor colorWithAlphaComponent:0.001];_window.opaque=NO;
  _window.collectionBehavior=NSWindowCollectionBehaviorMoveToActiveSpace|NSWindowCollectionBehaviorTransient|NSWindowCollectionBehaviorIgnoresCycle|NSWindowCollectionBehaviorFullScreenAuxiliary;
  _visuals=[[OCTSettingsVisuals alloc] initWithImage:arrow];
  NSView *content=[[NSView alloc] initWithFrame:NSMakeRect(0,0,531,110)];_window.contentView=content;
  NSView *material=_visuals.materialView;material.frame=content.bounds;material.autoresizingMask=NSViewWidthSizable|NSViewHeightSizable;[content addSubview:material];
  _drag=[[OCTSettingsDragView alloc] initWithFrame:NSZeroRect];_drag.translatesAutoresizingMaskIntoConstraints=NO;[content addSubview:_drag];
  NSBox *row=[[NSBox alloc] initWithFrame:NSZeroRect];row.translatesAutoresizingMaskIntoConstraints=NO;row.boxType=NSBoxCustom;row.titlePosition=NSNoTitle;row.cornerRadius=7;row.contentViewMargins=NSMakeSize(4,5);row.fillColor=NSColor.controlBackgroundColor;row.borderColor=NSColor.separatorColor;
  NSStackView *application=[NSStackView new];application.orientation=NSUserInterfaceLayoutOrientationHorizontal;application.spacing=4;
  id nativeBundle=helper[@"bundle"];
  NSImage *icon=nativeBundle!=NSNull.null?[NSWorkspace.sharedWorkspace iconForFile:nativeBundle[@"path"]]:[NSImage imageNamed:NSImageNameApplicationIcon];
  if(!icon){Fail(error,@"The native application icon is unavailable.");[self invalidate];return nil;}
  icon=[icon copy];icon.size=NSMakeSize(32,32);NSImageView *iconView=[NSImageView imageViewWithImage:icon];
  [iconView.widthAnchor constraintEqualToConstant:32].active=YES;[iconView.heightAnchor constraintEqualToConstant:32].active=YES;
  [application addArrangedSubview:iconView];[application addArrangedSubview:Label(helper[@"display_name"],[NSFont systemFontOfSize:NSFont.systemFontSize],NSColor.labelColor)];
  row.contentView=application;[row.widthAnchor constraintEqualToConstant:application.fittingSize.width+2*row.contentViewMargins.width].active=YES;[_drag addSubview:row];_drag.applicationRow=row;
  NSTextField *dragLabel=Label(@"Drag this application",[NSFont systemFontOfSize:NSFont.smallSystemFontSize],NSColor.secondaryLabelColor);dragLabel.translatesAutoresizingMaskIntoConstraints=NO;[_drag addSubview:dragLabel];
  NSString *instruction=nativeBundle==NSNull.null?[NSString stringWithFormat:@"Enable %@ for the requesting process in System Settings. This process has no native application bundle to drag.",permission[@"title"]]:[NSString stringWithFormat:@"Drag %@ to the list above to allow %@.",helper[@"display_name"],permission[@"title"]];
  NSTextField *hint=Label(instruction,[NSFont systemFontOfSize:NSFont.systemFontSize],NSColor.secondaryLabelColor);hint.translatesAutoresizingMaskIntoConstraints=NO;[content addSubview:hint];
  [NSLayoutConstraint activateConstraints:@[
    [_drag.leadingAnchor constraintEqualToAnchor:content.leadingAnchor constant:18],[_drag.bottomAnchor constraintEqualToAnchor:content.bottomAnchor],[_drag.topAnchor constraintEqualToAnchor:content.topAnchor],
    [row.leadingAnchor constraintEqualToAnchor:_drag.leadingAnchor],[row.trailingAnchor constraintEqualToAnchor:_drag.trailingAnchor],
    [row.bottomAnchor constraintEqualToAnchor:_drag.bottomAnchor constant:-20],[row.heightAnchor constraintEqualToConstant:42],
    [dragLabel.leadingAnchor constraintEqualToAnchor:_drag.leadingAnchor constant:4],[dragLabel.trailingAnchor constraintLessThanOrEqualToAnchor:_drag.trailingAnchor],[dragLabel.bottomAnchor constraintEqualToAnchor:row.topAnchor constant:-7],
    [hint.leadingAnchor constraintEqualToAnchor:_drag.trailingAnchor constant:16],[hint.bottomAnchor constraintEqualToAnchor:content.bottomAnchor constant:-27],
    [hint.trailingAnchor constraintLessThanOrEqualToAnchor:content.trailingAnchor]
  ]];
  [_drag setContentHuggingPriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationHorizontal];
  if(nativeBundle==NSNull.null)dragLabel.stringValue=helper[@"executable_path"];
  _arrowWindow=[[OCTSettingsWindow alloc] initWithContentRect:NSMakeRect(0,0,100,100) styleMask:NSWindowStyleMaskBorderless backing:NSBackingStoreBuffered defer:NO];
  _arrowWindow.releasedWhenClosed=NO;_arrowWindow.backgroundColor=NSColor.clearColor;_arrowWindow.opaque=NO;_arrowWindow.hasShadow=NO;_arrowWindow.level=NSFloatingWindowLevel;_arrowWindow.collectionBehavior=NSWindowCollectionBehaviorManaged;
  NSView *arrowView=_visuals.arrowView;arrowView.frame=NSMakeRect(0,0,100,100);_arrowWindow.contentView=arrowView;
  [_window addChildWindow:_arrowWindow ordered:NSWindowAbove];
  __weak OCTSettingsAccessory *weak=self;
  _drag.currentURL=^NSURL *{OCTSettingsAccessory *strong=weak;if(!strong||strong->_invalidated||!strong->_source||!SameProcess(strong->_helperIncarnation)||NSEqualRects(SettingsRect(strong->_source[@"settings"],strong->_source[@"window"][@"id"]),NSZeroRect))return nil;
    NSDictionary *actual=ProcessIdentity([strong->_helper[@"process_id"] intValue],nil);id bundle=actual[@"bundle"];
    if(!actual||![actual[@"executable_path"] isEqual:strong->_helper[@"executable_path"]]||bundle==NSNull.null||![bundle isEqual:strong->_helper[@"bundle"]])return nil;
    return [NSURL fileURLWithPath:bundle[@"path"] isDirectory:YES];};
  _drag.dropped=^(NSPoint point){OCTSettingsAccessory *strong=weak;if(!strong||!strong->_source)return;
    NSScreen *screen=NSScreen.screens.firstObject;NSRect source=SettingsRect(strong->_source[@"settings"],strong->_source[@"window"][@"id"]);
    if(screen&&!NSEqualRects(source,NSZeroRect)&&NSPointInRect(NSMakePoint(point.x,screen.frame.size.height-point.y),source))[strong hide];};
  _timer=[NSTimer scheduledTimerWithTimeInterval:0.1 repeats:YES block:^(NSTimer *timer){[weak layout];}];
  return self;
}
- (BOOL)update:(NSDictionary *)source error:(NSError **)error {
  if(_invalidated||!NativeProcess(source[@"settings"]))return Fail(error,@"The Settings accessory native source is invalid.");
  if(_source&&(![_source[@"settings"] isEqual:source[@"settings"]]||![_source[@"window"][@"id"] isEqual:source[@"window"][@"id"]]))return Fail(error,@"The Settings accessory cannot retarget its selected native window.");
  _source=[source copy];_application=[NSRunningApplication runningApplicationWithProcessIdentifier:[source[@"settings"][@"process_id"] intValue]];[self layout];return YES;
}
- (void)layout {
  if(_invalidated||!_source||!_application||_application.terminated||!SameProcess(_helperIncarnation)){[self hide];return;}
  NSRect rect=SettingsRect(_source[@"settings"],_source[@"window"][@"id"]);NSScreen *screen=NSScreen.screens.firstObject;
  if(NSEqualRects(rect,NSZeroRect)||!screen){[_visuals setPresented:NO];[_window orderOut:nil];[_arrowWindow orderOut:nil];return;}
  NSSize size=NSMakeSize(531,110);
  NSRect content=NSIntegralRect(NSMakeRect(NSMaxX(rect)-size.width-10,screen.frame.size.height-NSMaxY(rect)+10,size.width,size.height));
  NSRect frame=NSIntegralRect([_window frameRectForContentRect:content]);
  if(!NSEqualRects(_window.frame,frame))[_window setFrame:frame display:YES];
  NSRect arrowFrame=NSMakeRect(NSMinX(_window.frame)+31,NSMinY(_window.frame)+60,100,100);
  if(!NSEqualRects(_arrowWindow.frame,arrowFrame))[_arrowWindow setFrame:arrowFrame display:YES];
  if(!_window.visible)[_window orderFrontRegardless];if(!_arrowWindow.visible)[_arrowWindow orderFrontRegardless];[_visuals setPresented:YES];
}
- (void)hide { _source=nil;[_visuals setPresented:NO];[_window orderOut:nil];[_arrowWindow orderOut:nil]; }
- (void)invalidate { if(_invalidated)return;_invalidated=YES;[_timer invalidate];_timer=nil;_drag.currentURL=nil;_drag.dropped=nil;[self hide];[_window removeChildWindow:_arrowWindow];[_arrowWindow close];[_window close]; }
- (void)dealloc { [_timer invalidate];[_visuals setPresented:NO]; }
@end
@interface OCTPermissionButton : NSButton
@property(nonatomic,copy) NSString *permissionID;
@property(nonatomic,copy) NSString *operation;
@end
@implementation OCTPermissionButton
@end

@implementation OCTPermissionWindowController {
  __weak NSWindow *_owner;
  NSString *_identifier,*_generation;
  NSDictionary *_snapshot;
  void (^_event)(NSDictionary *);
  NSStackView *_rows;
  NSTextField *_errorLabel;
  NSButton *_refresh,*_close;
  uint64_t _sequence;
  BOOL _busy,_invalidated;
  id _activationObserver,_ownerObserver;
  NSBundle *_resourceBundle;
  OCTSettingsAccessory *_settingsAccessory;
  NSString *_settingsPermission,*_settingsRequest;
  NSDictionary *_settingsSource;
  BOOL _settingsEnded;
}
- (instancetype)initWithOwner:(NSWindow *)owner identifier:(NSString *)identifier generation:(NSString *)generation
                    snapshot:(NSDictionary *)snapshot resourceBundle:(NSBundle *)resourceBundle event:(void (^)(NSDictionary *))event error:(NSError **)error {
  if(![NSThread isMainThread] || !owner || ![NSApp.windows containsObject:owner] || !Text(identifier) || !Text(generation) || !event || !Snapshot(snapshot,error))return nil;
  NSWindow *window=[[NSWindow alloc] initWithContentRect:NSZeroRect styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable|NSWindowStyleMaskFullSizeContentView backing:NSBackingStoreBuffered defer:NO];
  if(!(self=[super initWithWindow:window]))return nil;
  _owner=owner;_identifier=[identifier copy];_generation=[generation copy];_snapshot=[snapshot copy];_event=[event copy];_resourceBundle=resourceBundle;
  window.delegate=self;window.title=@"Computer Use Permissions";window.titleVisibility=NSWindowTitleHidden;
  window.titlebarAppearsTransparent=YES;window.movableByWindowBackground=YES;window.releasedWhenClosed=NO;
  window.level=NSFloatingWindowLevel;
  NSView *content=[NSView new];window.contentView=content;
  NSStackView *stack=[NSStackView new];stack.orientation=NSUserInterfaceLayoutOrientationVertical;stack.alignment=NSLayoutAttributeLeading;stack.spacing=16;
  stack.translatesAutoresizingMaskIntoConstraints=NO;[content addSubview:stack];
  [NSLayoutConstraint activateConstraints:@[[stack.leadingAnchor constraintEqualToAnchor:content.leadingAnchor constant:24],
    [stack.trailingAnchor constraintEqualToAnchor:content.trailingAnchor constant:-24],
    [stack.topAnchor constraintEqualToAnchor:content.topAnchor constant:32],
    [stack.bottomAnchor constraintEqualToAnchor:content.bottomAnchor constant:-24],
    [stack.widthAnchor constraintEqualToConstant:464]]];
  [stack addArrangedSubview:Label(@"Allow Computer Use on this Mac",[NSFont systemFontOfSize:20 weight:NSFontWeightSemibold],NSColor.labelColor)];
  NSString *description=@"macOS permissions are separate from approving an app in Octopus and do not give a worker control of your Mac.";
  [stack addArrangedSubview:Label(description,[NSFont systemFontOfSize:13],NSColor.secondaryLabelColor)];
  NSDictionary *helper=snapshot[@"helper"];
  [stack addArrangedSubview:Label([NSString stringWithFormat:@"Requesting process: %@ (PID %@)\n%@",helper[@"display_name"],helper[@"process_id"],helper[@"executable_path"]],[NSFont monospacedSystemFontOfSize:11 weight:NSFontWeightRegular],NSColor.secondaryLabelColor)];
  if(helper[@"responsible_app"]!=NSNull.null)[stack addArrangedSubview:Label([NSString stringWithFormat:@"macOS responsible app: %@\n%@",helper[@"responsible_app"][@"identifier"],helper[@"responsible_app"][@"path"]],[NSFont systemFontOfSize:11],NSColor.secondaryLabelColor)];
  _rows=[NSStackView new];_rows.orientation=NSUserInterfaceLayoutOrientationVertical;_rows.alignment=NSLayoutAttributeLeading;_rows.spacing=16;
  [stack addArrangedSubview:_rows];[_rows.widthAnchor constraintEqualToAnchor:stack.widthAnchor].active=YES;
  _errorLabel=Label(@"",[NSFont systemFontOfSize:12],NSColor.systemRedColor);[stack addArrangedSubview:_errorLabel];
  NSStackView *footer=[NSStackView new];footer.orientation=NSUserInterfaceLayoutOrientationHorizontal;footer.spacing=10;
  _refresh=[NSButton buttonWithTitle:@"Refresh" target:self action:@selector(refresh:)];
  _close=[NSButton buttonWithTitle:@"Not now" target:self action:@selector(dismiss:)];_close.keyEquivalent=@"\e";
  [footer addArrangedSubview:_refresh];[footer addArrangedSubview:_close];[stack addArrangedSubview:footer];
  __weak OCTPermissionWindowController *weak=self;
  _activationObserver=[NSNotificationCenter.defaultCenter addObserverForName:NSApplicationDidBecomeActiveNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note){
    OCTPermissionWindowController *strong=weak;if(strong && strong.window.visible && !strong->_busy && !strong->_invalidated)[strong refresh:nil];
  }];
  _ownerObserver=[NSNotificationCenter.defaultCenter addObserverForName:NSWindowWillCloseNotification object:owner queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note){[weak invalidate];}];
  [self render];return self;
}
- (void)render {
  for(NSView *view in _rows.arrangedSubviews.copy){[_rows removeArrangedSubview:view];[view removeFromSuperview];}
  BOOL allGranted=YES;
  NSDictionary *titles=@{@"granted":@"Allowed",@"not_granted":@"Required",@"not_determined":@"Not requested",@"denied":@"Not allowed",@"restricted":@"Restricted by macOS",@"unavailable":@"Unavailable"};
  for(NSDictionary *row in [_snapshot[@"permissions"] arrayByAddingObjectsFromArray:_snapshot[@"automation"]]){
    NSStackView *line=[NSStackView new];line.orientation=NSUserInterfaceLayoutOrientationVertical;line.alignment=NSLayoutAttributeLeading;line.spacing=5;
    [line addArrangedSubview:Label(row[@"title"],[NSFont systemFontOfSize:14 weight:NSFontWeightSemibold],NSColor.labelColor)];
    if([row[@"kind"] isEqual:@"automation"]){
      NSDictionary *sender=row[@"provenance"][@"sender"],*recipient=row[@"provenance"][@"recipient"];
      NSString *identity=[NSString stringWithFormat:@"Sender: %@ (PID %@)\n%@\nRecipient: %@ (PID %@)\n%@",sender[@"display_name"],sender[@"process_id"],sender[@"executable_path"],recipient[@"display_name"],recipient[@"process_id"],recipient[@"executable_path"]];
      [line addArrangedSubview:Label(identity,[NSFont monospacedSystemFontOfSize:11 weight:NSFontWeightRegular],NSColor.secondaryLabelColor)];
    }
    BOOL granted=[row[@"status"] isEqual:@"granted"];allGranted&=granted;
    [line addArrangedSubview:Label(titles[row[@"status"]],[NSFont systemFontOfSize:12],granted?NSColor.systemGreenColor:NSColor.secondaryLabelColor)];
    if(Text(row[@"detail"]))[line addArrangedSubview:Label(row[@"detail"],[NSFont systemFontOfSize:12],NSColor.secondaryLabelColor)];
    NSStackView *actions=[NSStackView new];actions.orientation=NSUserInterfaceLayoutOrientationHorizontal;actions.spacing=8;
    for(NSString *operation in @[@"request",@"open_settings"]){
      BOOL available=[row[ [operation isEqual:@"request"]?@"can_request":@"can_open_settings"] boolValue];
      if(!available || (granted && [operation isEqual:@"request"]))continue;
      OCTPermissionButton *button=[OCTPermissionButton buttonWithTitle:[operation isEqual:@"request"]?@"Allow…":@"Open System Settings" target:self action:@selector(act:)];
      button.permissionID=row[@"id"];button.operation=operation;button.enabled=!_busy;[actions addArrangedSubview:button];
    }
    if(actions.arrangedSubviews.count)[line addArrangedSubview:actions];[_rows addArrangedSubview:line];
    [line.widthAnchor constraintEqualToAnchor:_rows.widthAnchor].active=YES;
  }
  _refresh.enabled=!_busy;_close.title=allGranted?@"Done":@"Not now";
  [self.window.contentView layoutSubtreeIfNeeded];[self.window setContentSize:self.window.contentView.fittingSize];
}
- (BOOL)ownerAvailable { return !_invalidated && _owner && [NSApp.windows containsObject:_owner]; }
- (void)emitOperation:(NSString *)operation permission:(nullable NSString *)permission {
  if(![self ownerAvailable]){[self invalidate];return;}
  if(_busy)return;
  if(_sequence>=9007199254740991ULL){[self invalidate];return;}
  ++_sequence;_busy=YES;_errorLabel.stringValue=@"";
  [_settingsAccessory invalidate];_settingsAccessory=nil;_settingsSource=nil;_settingsRequest=nil;_settingsEnded=NO;
  _settingsPermission=[operation isEqual:@"open_settings"]?[permission copy]:nil;[self render];
  _event(@{@"type":@"permission-action",@"presentationId":_identifier,@"generation":_generation,@"sequence":@(_sequence),@"operation":operation,@"permission":permission?:NSNull.null});
}
- (void)act:(OCTPermissionButton *)button {
  NSDictionary *row=nil;for(NSDictionary *candidate in [_snapshot[@"permissions"] arrayByAddingObjectsFromArray:_snapshot[@"automation"]])if([candidate[@"id"] isEqual:button.permissionID]){row=candidate;break;}
  NSString *key=[button.operation isEqual:@"request"]?@"can_request":@"can_open_settings";
  if(row && [row[key] boolValue])[self emitOperation:button.operation permission:button.permissionID];
}
- (void)refresh:(nullable id)sender { [self emitOperation:@"status" permission:nil]; }
- (void)dismiss:(nullable id)sender { [self.window close]; }
- (BOOL)update:(NSDictionary *)snapshot sequence:(uint64_t)sequence errorText:(nullable NSString *)errorText error:(NSError **)error {
  if(![NSThread isMainThread] || ![self ownerAvailable] || sequence!=_sequence)return Fail(error,@"The permission window request is no longer current.");
  if(!Snapshot(snapshot,error) || ![snapshot[@"host_generation"] isEqual:_snapshot[@"host_generation"]]
      || ![snapshot[@"helper"] isEqual:_snapshot[@"helper"]])return Fail(error,@"The permission helper changed; open a new permission window.");
  [_settingsAccessory invalidate];_settingsAccessory=nil;_settingsPermission=nil;_settingsSource=nil;_settingsRequest=nil;
  _snapshot=[snapshot copy];_busy=NO;_errorLabel.stringValue=errorText?:@"";[self render];return YES;
}
- (BOOL)updateSettingsSource:(NSDictionary *)source sequence:(uint64_t)sequence error:(NSError **)error {
  if(!NSThread.isMainThread||![self ownerAvailable]||!_busy||!_settingsPermission||sequence!=_sequence)return Fail(error,@"The human Settings action is no longer pending.");
  if(!source){[_settingsAccessory hide];return YES;}
  if(!Keys(source,@[@"schema_version",@"host_generation",@"permission_request_id",@"permission",@"state",@"settings",@"window"])
    || ![source[@"schema_version"] isEqual:@1]||![source[@"host_generation"] isEqual:_snapshot[@"host_generation"]]||!Text(source[@"permission_request_id"])
    || (_settingsRequest&&![_settingsRequest isEqual:source[@"permission_request_id"]])||![@[@"waiting",@"observed",@"ended"] containsObject:source[@"state"]]
    || (![_settingsPermission isEqual:source[@"permission"]]&&!([source[@"state"] isEqual:@"ended"]&&source[@"permission"]==NSNull.null)))return Fail(error,@"The Settings witness changed its original request.");
  if(!_settingsRequest)_settingsRequest=[source[@"permission_request_id"] copy];
  if(![source[@"state"] isEqual:@"observed"]){
    if(source[@"settings"]!=NSNull.null||source[@"window"]!=NSNull.null)return Fail(error,@"An absent Settings witness contains a native window.");
    if([source[@"state"] isEqual:@"ended"]&&(_settingsSource||source[@"permission"]!=NSNull.null))_settingsEnded=YES;
    [_settingsAccessory hide];return YES;
  }
  NSDictionary *window=source[@"window"],*bounds=window[@"bounds"];
  if(_settingsEnded||!NativeProcess(source[@"settings"])||!Keys(window,@[@"id",@"observation_sequence",@"bounds"])||![window[@"id"] isKindOfClass:NSNumber.class]||CFGetTypeID((__bridge CFTypeRef)window[@"id"])==CFBooleanGetTypeID()||[window[@"id"] doubleValue]<=0||[window[@"id"] doubleValue]>4294967295.0||std::floor([window[@"id"] doubleValue])!=[window[@"id"] doubleValue]
    || ![window[@"observation_sequence"] isKindOfClass:NSNumber.class]||CFGetTypeID((__bridge CFTypeRef)window[@"observation_sequence"])==CFBooleanGetTypeID()
    || [window[@"observation_sequence"] doubleValue]<1||[window[@"observation_sequence"] doubleValue]>9007199254740991.0||std::floor([window[@"observation_sequence"] doubleValue])!=[window[@"observation_sequence"] doubleValue]
    || !Keys(bounds,@[@"x",@"y",@"width",@"height"]))return Fail(error,@"The Settings witness is not a current native source.");
  for(NSString *key in bounds)if(![bounds[key] isKindOfClass:NSNumber.class]||CFGetTypeID((__bridge CFTypeRef)bounds[key])==CFBooleanGetTypeID()||!std::isfinite([bounds[key] doubleValue]))return Fail(error,@"The Settings witness bounds are invalid.");
  if([bounds[@"width"] doubleValue]<=0||[bounds[@"height"] doubleValue]<=0)return Fail(error,@"The Settings witness has no native area.");
  if(_settingsSource&&(![_settingsSource[@"settings"] isEqual:source[@"settings"]]||![_settingsSource[@"window"][@"id"] isEqual:window[@"id"]]
    || [window[@"observation_sequence"] unsignedLongLongValue]<[_settingsSource[@"window"][@"observation_sequence"] unsignedLongLongValue]
    || ([window[@"observation_sequence"] isEqual:_settingsSource[@"window"][@"observation_sequence"]]&&![_settingsSource[@"window"] isEqual:window])))return Fail(error,@"The Settings witness replaced its retained native source.");
  if(!_settingsAccessory){
    NSDictionary *permission=nil;for(NSDictionary *row in _snapshot[@"permissions"])if([row[@"id"] isEqual:_settingsPermission])permission=row;
    if(!permission)return Fail(error,@"The native helper requirement is no longer available.");
    _settingsAccessory=[[OCTSettingsAccessory alloc] initWithHelper:_snapshot[@"helper"] permission:permission resourceBundle:_resourceBundle error:error];if(!_settingsAccessory)return NO;
  }
  if(![_settingsAccessory update:source error:error])return NO;_settingsSource=[source copy];return YES;
}
- (BOOL)hasGeneration:(NSString *)generation { return [_generation isEqual:generation]; }
- (void)present {
  if(![self ownerAvailable])return;
  if(!self.window.visible)[self.window center];
  [self showWindow:nil];[NSApp activateIgnoringOtherApps:YES];[self.window makeKeyAndOrderFront:nil];
}
- (void)windowWillClose:(NSNotification *)notification {
  if(_invalidated)return;
  void (^event)(NSDictionary *)=[_event copy];NSDictionary *value=@{@"type":@"permission-action",@"presentationId":_identifier,@"generation":_generation,@"sequence":@(_sequence),@"operation":@"dismiss",@"permission":NSNull.null};
  [self invalidate];event(value);
}
- (void)invalidate {
  if(_invalidated)return;_invalidated=YES;_event=nil;[_settingsAccessory invalidate];_settingsAccessory=nil;
  if(_activationObserver)[NSNotificationCenter.defaultCenter removeObserver:_activationObserver];
  if(_ownerObserver)[NSNotificationCenter.defaultCenter removeObserver:_ownerObserver];
  _activationObserver=nil;_ownerObserver=nil;self.window.delegate=nil;[self.window close];
}
- (void)dealloc {
  if(_activationObserver)[NSNotificationCenter.defaultCenter removeObserver:_activationObserver];
  if(_ownerObserver)[NSNotificationCenter.defaultCenter removeObserver:_ownerObserver];
  [_settingsAccessory invalidate];
}
@end

namespace {
NSDictionary *ProcessIdentity(pid_t pid, NSError **error) {
  struct proc_bsdinfo info{}; char executable[PROC_PIDPATHINFO_MAXSIZE]{};
  if(pid<=0 || proc_pidinfo(pid,PROC_PIDTBSDINFO,0,&info,sizeof(info))!=sizeof(info)
      || proc_pidpath(pid,executable,sizeof(executable))<=0){Fail(error,@"The native permission process is no longer available.");return nil;}
  NSString *path=[[NSString alloc] initWithUTF8String:executable];
  const char *name=info.pbi_name[0]?info.pbi_name:info.pbi_comm;
  size_t capacity=info.pbi_name[0]?sizeof(info.pbi_name):sizeof(info.pbi_comm);
  NSString *display=[[NSString alloc] initWithBytes:name length:strnlen(name,capacity) encoding:NSUTF8StringEncoding];
  if(!Text(path)||![path isAbsolutePath]||!Text(display)){Fail(error,@"The native permission process identity is invalid.");return nil;}
  NSRunningApplication *app=[NSRunningApplication runningApplicationWithProcessIdentifier:pid];
  id bundle=app.bundleIdentifier.length&&app.bundleURL.path.length?@{@"identifier":app.bundleIdentifier,@"path":app.bundleURL.path}:NSNull.null;
  return @{@"process_id":@(pid),@"executable_path":path,@"display_name":display,@"bundle":bundle,@"responsible_app":NSNull.null,
    @"start_seconds":[NSString stringWithFormat:@"%llu",(unsigned long long)info.pbi_start_tvsec],
    @"start_microseconds":[NSString stringWithFormat:@"%llu",(unsigned long long)info.pbi_start_tvusec]};
}
BOOL SameProcess(NSDictionary *expected) {
  struct proc_bsdinfo info{};char path[PROC_PIDPATHINFO_MAXSIZE]{};pid_t pid=[expected[@"process_id"] intValue];
  return pid>0 && proc_pidinfo(pid,PROC_PIDTBSDINFO,0,&info,sizeof(info))==sizeof(info) && proc_pidpath(pid,path,sizeof(path))>0
    && info.pbi_start_tvsec==[expected[@"start_seconds"] longLongValue] && info.pbi_start_tvusec==[expected[@"start_microseconds"] longLongValue]
    && [expected[@"executable_path"] isEqualToString:[NSString stringWithUTF8String:path]];
}
}
@implementation OCTAutomationPermission {
  NSString *_identifier;
  NSDictionary *_sender,*_recipient;
  OSStatus _status;
  BOOL _observed,_pending,_settingsChecking;
  NSRunningApplication *_settingsApplication;
  NSDictionary *_settingsIdentity;
  NSString *_settingsOperation;
  NSTimer *_settingsTimer;
  void (^_settingsCompletion)(NSDictionary *,NSError *);
  std::shared_ptr<std::atomic_bool> _cancelled;
}
- (instancetype)initWithRecipientProcessID:(pid_t)pid identifier:(NSString *)identifier error:(NSError **)error {
  if(![NSThread isMainThread]||pid==getpid()||!Text(identifier))return nil;
  NSDictionary *sender=ProcessIdentity(getpid(),error),*recipient=ProcessIdentity(pid,error);if(!sender||!recipient)return nil;
  if(!(self=[super init]))return nil;
  _identifier=[identifier copy];_sender=sender;_recipient=recipient;_cancelled=std::make_shared<std::atomic_bool>(false);return self;
}
- (NSDictionary *)recipientIdentity { return @{@"process_id":_recipient[@"process_id"],@"executable_path":_recipient[@"executable_path"],@"start_seconds":_recipient[@"start_seconds"],@"start_microseconds":_recipient[@"start_microseconds"]}; }
- (BOOL)isCurrent { return !_cancelled->load() && SameProcess(_sender) && SameProcess(_recipient); }
- (NSDictionary *)snapshot {
  BOOL current=[self isCurrent];NSString *status=!current||!_observed?@"unavailable":_status==noErr?@"granted":_status==errAEEventNotPermitted?@"denied":_status==errAEEventWouldRequireUserConsent?@"not_determined":@"unavailable";
  NSString *detail=!current?@"The native sender or recipient process ended.":_pending?@"A macOS permission operation is still pending; it will not be repeated.":!_observed?@"The native Automation status has not been observed.":([status isEqual:@"unavailable"]?[NSString stringWithFormat:@"Native Automation status: %d",(int)_status]:@"This permission connects the client’s native presentation receiver to the existing helper. It does not approve any app or acquire input control.");
  return @{@"id":_identifier,@"kind":@"automation",@"title":@"Native presentation connection",@"status":status,
    @"can_request":@(current&&_observed&&!_pending&&_status==errAEEventWouldRequireUserConsent),@"can_open_settings":@(current&&_observed&&!_pending),@"detail":detail,
    @"provenance":@{@"api":@"AEDeterminePermissionToAutomateTarget",@"event_class":@"OcPi",@"event_id":@"BtV1",@"sender":_sender,@"recipient":_recipient}};
}
- (void)finishSettings:(NSError *)failure {
  void (^completion)(NSDictionary *,NSError *)=[_settingsCompletion copy];
  _settingsCompletion=nil;[_settingsTimer invalidate];_settingsTimer=nil;_settingsApplication=nil;_settingsIdentity=nil;_settingsOperation=nil;_settingsChecking=NO;_pending=NO;
  if(completion)completion(failure?nil:[self snapshot],failure);
}
- (void)settingsFailure:(NSString *)message {
  [self finishSettings:[NSError errorWithDomain:@"OctopusNativePermissions" code:1 userInfo:@{NSLocalizedDescriptionKey:message}]];
}
- (void)checkSettings {
  if(!_settingsCompletion||_settingsChecking)return;
  if(![self isCurrent]||!_settingsApplication||_settingsApplication.terminated||!SameProcess(_settingsIdentity)){
    [self settingsFailure:@"The exact native Settings or permission process ended before the permission was observed."];return;
  }
  _settingsChecking=YES;NSString *operation=[_settingsOperation copy];NSDictionary *sender=_sender,*recipient=_recipient;
  auto cancelled=_cancelled;__weak OCTAutomationPermission *weak=self;
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^{
    @autoreleasepool {
      OSStatus result=procNotFound;
      if(!cancelled->load()&&SameProcess(sender)&&SameProcess(recipient)){
        NSAppleEventDescriptor *target=[NSAppleEventDescriptor descriptorWithProcessIdentifier:[recipient[@"process_id"] intValue]];
        result=AEDeterminePermissionToAutomateTarget(target.aeDesc,'OcPi','BtV1',false);
      }
      dispatch_async(dispatch_get_main_queue(),^{
        OCTAutomationPermission *strong=weak;
        if(!strong||![strong->_settingsOperation isEqual:operation]||!strong->_settingsCompletion)return;
        strong->_settingsChecking=NO;
        if(![strong isCurrent]||strong->_settingsApplication.terminated||!SameProcess(strong->_settingsIdentity)){
          [strong settingsFailure:@"The native Settings or permission process changed during observation."];return;
        }
        strong->_observed=YES;strong->_status=result;
        if(result==noErr)[strong finishSettings:nil];
        else if(result!=errAEEventNotPermitted&&result!=errAEEventWouldRequireUserConsent)[strong settingsFailure:[NSString stringWithFormat:@"Native Automation status became unavailable: %d",(int)result]];
      });
    }
  });
}
- (void)cancelSettings:(NSString *)operationID {
  if(!NSThread.isMainThread||![_settingsOperation isEqual:operationID])return;
  [self settingsFailure:@"The Settings observation was cancelled. Settings may already be open; read the actual permission status before another request."];
}
- (BOOL)begin:(NSString *)operation operationID:(NSString *)operationID timeoutMilliseconds:(uint64_t)timeout completion:(void (^)(NSDictionary *,NSError *))completion error:(NSError **)error {
  if(![NSThread isMainThread]||![self isCurrent]||!completion||!Text(operationID)||!timeout||timeout>600000||![@[@"status",@"request",@"open_settings"] containsObject:operation])return Fail(error,@"The native Automation source is unavailable.");
  if(_pending){if([operation isEqual:@"status"]){completion([self snapshot],nil);return YES;}return Fail(error,@"A macOS permission request is already pending; refresh its status.");}
  if([operation isEqual:@"request"]&&(!_observed||_status!=errAEEventWouldRequireUserConsent))return Fail(error,@"This native Automation request is not available; review System Settings.");
  if([operation isEqual:@"open_settings"]){
    if(!_observed)return Fail(error,@"Observe the native Automation status before opening its settings.");
    NSURL *url=[NSURL URLWithString:@"x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Automation"];
    NSWorkspaceOpenConfiguration *configuration=[NSWorkspaceOpenConfiguration configuration];configuration.activates=YES;
    _pending=YES;_settingsOperation=[operationID copy];_settingsCompletion=[completion copy];
    __weak OCTAutomationPermission *weak=self;NSString *expected=[operationID copy];
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,static_cast<int64_t>(timeout)*NSEC_PER_MSEC),dispatch_get_main_queue(),^{
      OCTAutomationPermission *strong=weak;if(strong&&[strong->_settingsOperation isEqual:expected])
        [strong settingsFailure:@"The explicit Automation Settings observation timed out. Settings may already be open; refresh the actual status without repeating the request."];
    });
    [NSWorkspace.sharedWorkspace openURL:url configuration:configuration completionHandler:^(NSRunningApplication *application,NSError *failure){
      dispatch_async(dispatch_get_main_queue(),^{
        OCTAutomationPermission *strong=weak;if(!strong||![strong->_settingsOperation isEqual:expected])return;
        if(failure){[strong finishSettings:failure];return;}
        if(![strong isCurrent]||!application||application.terminated){[strong settingsFailure:@"macOS did not retain a live Settings application for the explicit request."];return;}
        NSError *identityError=nil;NSDictionary *identity=ProcessIdentity(application.processIdentifier,&identityError);
        if(!identity||identityError||![application.executableURL.path isEqual:identity[@"executable_path"]]){[strong settingsFailure:@"The opened Settings process identity could not be observed."];return;}
        strong->_settingsApplication=application;strong->_settingsIdentity=identity;
        strong->_settingsTimer=[NSTimer scheduledTimerWithTimeInterval:0.1 repeats:YES block:^(NSTimer *timer){[weak checkSettings];}];
        [strong checkSettings];
      });
    }];return YES;
  }
  _pending=YES;BOOL ask=[operation isEqual:@"request"];NSDictionary *sender=_sender,*recipient=_recipient;
  auto cancelled=_cancelled;__weak OCTAutomationPermission *weak=self;
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0), ^{
    @autoreleasepool {
      OSStatus result=procNotFound;
      if(!cancelled->load() && SameProcess(sender) && SameProcess(recipient)){
        NSAppleEventDescriptor *target=[NSAppleEventDescriptor descriptorWithProcessIdentifier:[recipient[@"process_id"] intValue]];
        result=AEDeterminePermissionToAutomateTarget(target.aeDesc,'OcPi','BtV1',ask);
      }
      dispatch_async(dispatch_get_main_queue(), ^{
        OCTAutomationPermission *strong=weak;
        if(!strong||cancelled->load())return;
        strong->_pending=NO;
        if(![strong isCurrent]){completion(nil,[NSError errorWithDomain:@"OctopusNativePermissions" code:1 userInfo:@{NSLocalizedDescriptionKey:@"The native permission process changed during the operation."}]);return;}
        strong->_observed=YES;strong->_status=result;completion([strong snapshot],nil);
      });
    }
  });return YES;
}
- (void)invalidate { _cancelled->store(true);_settingsCompletion=nil;[_settingsTimer invalidate];_settingsTimer=nil;_settingsOperation=nil;_settingsApplication=nil;_settingsIdentity=nil; }
- (void)dealloc { if(_cancelled)_cancelled->store(true);[_settingsTimer invalidate]; }
@end
