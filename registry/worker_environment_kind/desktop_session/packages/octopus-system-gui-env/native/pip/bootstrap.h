#ifndef OCTOPUS_NATIVE_PIP_BOOTSTRAP_H
#define OCTOPUS_NATIVE_PIP_BOOTSTRAP_H
#include "receiver.h"
#import "transport.h"
#include <functional>
#include <memory>

namespace octopus::pip {
class ReceiverBootstrap : public std::enable_shared_from_this<ReceiverBootstrap> {
 public:
  using OnEvent = std::function<void(NSDictionary*)>;
  static std::shared_ptr<ReceiverBootstrap> Create(OCTPIPStackControlText* text,
                                                  NSURL* asset, OnEvent on_event);
  ~ReceiverBootstrap();
  ReceiverBootstrap(const ReceiverBootstrap&) = delete;
  ReceiverBootstrap& operator=(const ReceiverBootstrap&) = delete;
  NSDictionary* BeginSource(NSDictionary* expected);
  void InstallSource(NSString* request_id, NSData* binding_bytes, NSData* key);
  void CancelSource(NSString* request_id);
  NSDictionary* PrepareAutomationPermission(NSString* request_id);
  void BeginAutomationPermission(NSString* request_id, NSString* operation_id, NSString* operation, uint64_t timeout_ms);
  void CancelAutomationSettings(NSString* request_id, NSString* operation_id);
  void UpsertBrowserImage(NSDictionary* identity, NSString* group_id,
                          NSString* batch_id, uint64_t sequence, NSData* image);
  void InvalidateBrowserImage(NSDictionary* identity);
  void CompleteGroup(NSString* group_id);
  void ClearCompletedGroup(NSString* group_id);
  void RevokeSource(NSDictionary* scope, NSString* window_id);
  StackHostOwner& Hosts();
  OCTPIPStack* Stack();
  NSDictionary* Status() const;
  void Stop();
  void Dispatch(NSXPCConnection* sender, NSDictionary* identity,
                std::function<void(Receiver&)> action,
                void (^reply)(NSError*));
  void WillEnd(NSXPCConnection* sender, NSDictionary* identity, void (^reply)(NSError*));
  void Invalidate(NSXPCConnection* sender, NSDictionary* identity,
                  NSString* reason, void (^reply)(NSError*));
 private:
  ReceiverBootstrap();
  void Initialize(OCTPIPStackControlText* text, NSURL* asset, OnEvent on_event);
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
}
#endif
