#ifndef OCTOPUS_NATIVE_PIP_RECEIVER_H
#define OCTOPUS_NATIVE_PIP_RECEIVER_H

#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <xpc/xpc.h>
#import "stack.h"
#include <cstddef>
#include <cstdint>
#include <functional>
#include <memory>
#include <string>

namespace octopus::pip {
constexpr uint64_t kMaxSequence = 9007199254740991ULL;
constexpr size_t kMaxMetadataBytes = 8 * 1024 * 1024;

struct Identity {
  std::string host_generation;
  std::string presentation_id;
  uint64_t stream_generation = 0;
  bool operator==(const Identity&) const = default;
};

enum class OperationKind { Resize, ResizeInProgress, ReplaceContext };

class SendRight {
 public:
  static SendRight CopyFence(xpc_object_t payload);
  SendRight(SendRight&& other) noexcept;
  SendRight& operator=(SendRight&& other) noexcept;
  ~SendRight();
  mach_port_t get() const { return port_; }
  SendRight(const SendRight&) = delete;
  SendRight& operator=(const SendRight&) = delete;
 private:
  explicit SendRight(mach_port_t port) : port_(port) {}
  mach_port_t port_ = MACH_PORT_NULL;
};

struct OperationState {
  uint32_t context_id = 0;
  CGSize source_size = CGSizeZero;
  uint64_t completed = 0;
  uint64_t pending = 0;
  void Validate(uint64_t operation, OperationKind kind, uint32_t context,
                CGSize size) const;
  void Prepared(uint64_t operation, OperationKind kind, uint32_t context,
                CGSize size);
  void Complete(uint64_t operation);
};

class LayerAttachment {
 public:
  LayerAttachment(CALayer* parent, CGRect frame, uint32_t context_id);
  ~LayerAttachment();
  LayerAttachment(const LayerAttachment&) = delete;
  LayerAttachment& operator=(const LayerAttachment&) = delete;
  void Prepare(OperationKind kind, uint32_t context_id, CGSize display_size,
               const SendRight& fence);
  void SetFrame(CGRect frame, bool scale_from_committed_size, bool visible);
  void SetFrameInCurrentTransaction(CGRect frame, bool scale_from_committed_size,
                                   bool visible);
  void Clear();
  void Detach();
  CALayer* layer() const;
 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

id<OCTPIPStackAttachment> WrapAttachment(std::shared_ptr<LayerAttachment> attachment);

struct Event {
  std::string type;
  Identity identity;
  uint64_t operation = 0;
  CGSize size = CGSizeZero;
};

class ReceiverBootstrap;
class StackHostOwner : public std::enable_shared_from_this<StackHostOwner> {
 public:
  explicit StackHostOwner(OCTPIPStack* stack);
  ~StackHostOwner();
  StackHostOwner(const StackHostOwner&) = delete;
  StackHostOwner& operator=(const StackHostOwner&) = delete;
  bool OwnsStack(OCTPIPStack* stack) const;
  bool HasHost(NSString* host_id, NSString* generation, NSWindow* window) const;
  void RegisterHost(const std::string& host_id, const std::string& generation,
                    const void* window_handle, CGRect anchor, CGRect passthrough,
                    NSArray<OCTPIPStackHostAnchor*>* anchors, OCTPIPStackScope scope,
                    OCTPIPStackHostRole role, bool home_available, bool animated);
  void RemoveHost(const std::string& host_id, const std::string& generation);
  void Close();
 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

class Receiver : public std::enable_shared_from_this<Receiver> {
 public:
  using OnEvent = std::function<void(const Event&)>;
  explicit Receiver(Identity expected, OnEvent on_event);
  ~Receiver();
  Receiver(const Receiver&) = delete;
  Receiver& operator=(const Receiver&) = delete;
  bool authenticated() const;
  bool published() const;
  bool MatchesIdentity(OCTPIPStackIdentity* identity) const;
  bool HasHost(NSString* host_id, NSString* generation, NSWindow* window) const;
  void RegisterHost(const std::string& host_id, const std::string& generation,
                    const void* window_handle, CGRect anchor, CGRect passthrough,
                    NSArray<OCTPIPStackHostAnchor*>* anchors, OCTPIPStackScope scope,
                    OCTPIPStackHostRole role, bool home_available, bool animated);
  id<OCTPIPStackAttachment> AttachToParent(CALayer* parent,
              const std::string& attachment_id, uint64_t generation, CGRect frame);
  void Detach(const std::string& attachment_id, uint64_t generation);
  void RemoveHost(const std::string& host_id, const std::string& generation);
  void Publish(NSXPCConnection* sender, const Identity& identity,
               uint32_t context_id, CGSize source_size);
  void Prepare(NSXPCConnection* sender, const Identity& identity,
               uint64_t operation, OperationKind kind, uint32_t context_id,
               CGSize source_size, SendRight fence);
  void Complete(NSXPCConnection* sender, const Identity& identity, uint64_t operation);
  void Invalidate(NSXPCConnection* sender, const Identity& identity);
  void End(NSXPCConnection* sender, const Identity& identity, std::function<void()> completion);
  void ShowCompletion();
  void ConnectionLost(NSXPCConnection* sender);
  void Stop();
 private:
  friend class ReceiverBootstrap;
  void InstallAuthenticatedConnection(NSXPCConnection* connection,
                    const Identity& verified_identity, OCTPIPStack* stack,
                    std::shared_ptr<StackHostOwner> host_owner,
                    NSString* group_id, NSString* batch_id);
  void CheckPeer(NSXPCConnection* sender, const Identity& identity) const;
  void Emit(const std::string& type, uint64_t operation = 0);
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
}
#endif
