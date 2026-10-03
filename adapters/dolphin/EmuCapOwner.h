// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <cstdint>
#include <optional>
#include <set>
#include <string>
#include <utility>

namespace EmuCap::Temporal
{
// Native counterpart of live::temporal::owner. Only the sole worker mutates this object.
// Transport authentication must happen before any identity is supplied here.
struct OperationKey
{
  std::string runtime, owner_id, operation_id;
  bool operator==(const OperationKey&) const = default;
  bool Valid() const
  {
    return ValidPart(runtime) && ValidPart(owner_id) && ValidPart(operation_id);
  }
  static bool ValidPart(const std::string& s) { return !s.empty() && s.size() <= 256; }
};
struct Attachment
{
  std::string broker_instance;
  uint64_t registration = 0, session = 0;
  bool operator==(const Attachment&) const = default;
  bool Valid() const
  {
    return OperationKey::ValidPart(broker_instance) && registration != 0 && session != 0;
  }
};
struct CleanupPlan
{
  OperationKey key;
  Attachment attachment;
  std::set<uint64_t> input_ports;
  bool effects_started = false;
  bool operator==(const CleanupPlan&) const = default;
};
struct CleanupEvidence
{
  bool stop_verified = false;
  std::set<uint64_t> released_ports;
};
enum class OwnerResult { Ok, InvalidIdentity, WrongAttachment, Busy, NotActive, Retired };

class ProducerOwnership
{
public:
  explicit ProducerOwnership(std::string runtime) : m_runtime(std::move(runtime)) {}
  OwnerResult Attach(const Attachment& attachment)
  {
    if (m_retired) return OwnerResult::Retired;
    if (!OperationKey::ValidPart(m_runtime) || !attachment.Valid())
      return OwnerResult::InvalidIdentity;
    if (m_attachment == attachment) return OwnerResult::Ok;
    if (m_parent || m_attachment) return OwnerResult::Busy;
    m_terminal.reset();
    m_attachment = attachment;
    return OwnerResult::Ok;
  }
  OwnerResult Begin(const Attachment& attachment, const OperationKey& key)
  {
    if (m_retired) return OwnerResult::Retired;
    if (!key.Valid() || key.runtime != m_runtime) return OwnerResult::InvalidIdentity;
    if (m_attachment != attachment) return OwnerResult::WrongAttachment;
    if (m_parent)
      return m_parent->key == key && !m_stopping ? OwnerResult::Ok : OwnerResult::Busy;
    if (m_terminal && m_terminal->key == key) return OwnerResult::NotActive;
    m_terminal.reset();
    m_parent = CleanupPlan{key, attachment, {}, false};
    m_stopping = false;
    return OwnerResult::Ok;
  }
  OwnerResult AuthorizeUnscoped(const Attachment& attachment, bool observation) const
  {
    if (m_retired) return OwnerResult::Retired;
    if (m_attachment != attachment) return OwnerResult::WrongAttachment;
    return m_parent && !observation ? OwnerResult::Busy : OwnerResult::Ok;
  }
  OwnerResult Authorize(const Attachment& attachment, const OperationKey& key) const
  {
    if (m_retired) return OwnerResult::Retired;
    if (m_attachment != attachment) return OwnerResult::WrongAttachment;
    return m_parent && m_parent->key == key && !m_stopping ? OwnerResult::Ok :
                                                                         OwnerResult::NotActive;
  }
  OwnerResult Effect(const Attachment& attachment, const OperationKey& key,
                     std::optional<uint64_t> input_port = std::nullopt)
  {
    const auto result = Authorize(attachment, key);
    if (result != OwnerResult::Ok) return result;
    if (input_port) m_parent->input_ports.insert(*input_port);
    m_parent->effects_started = true;
    return OwnerResult::Ok;
  }
  OwnerResult StartCleanup(const Attachment& attachment, const OperationKey& key,
                           CleanupPlan& plan)
  {
    if (m_retired) return OwnerResult::Retired;
    if (!m_parent || m_parent->key != key || m_parent->attachment != attachment)
      return OwnerResult::NotActive;
    m_stopping = true;
    plan = *m_parent;
    return OwnerResult::Ok;
  }
  OwnerResult Detach(const Attachment& attachment, std::optional<CleanupPlan>& plan)
  {
    plan.reset();
    if (m_retired) return OwnerResult::Retired;
    if (m_attachment != attachment) return OwnerResult::Ok;
    m_attachment.reset();
    m_terminal.reset();
    if (m_parent)
    {
      m_stopping = true;
      plan = m_parent;
    }
    return OwnerResult::Ok;
  }
  OwnerResult FinishCleanup(const CleanupPlan& plan, const CleanupEvidence& evidence)
  {
    if (m_retired) return OwnerResult::Retired;
    if (!m_parent || !m_stopping || *m_parent != plan) return OwnerResult::NotActive;
    if ((plan.effects_started && !evidence.stop_verified) ||
        evidence.released_ports != plan.input_ports)
    {
      m_retired = true;
      return OwnerResult::Retired;
    }
    if (m_attachment == plan.attachment) m_terminal = plan;
    m_parent.reset();
    return OwnerResult::Ok;
  }
  const CleanupPlan* Terminal(const Attachment& attachment, const OperationKey& key) const
  {
    return m_terminal && m_terminal->attachment == attachment && m_terminal->key == key ?
               &*m_terminal : nullptr;
  }
  std::optional<OperationKey> ActiveKey() const
  {
    return m_parent ? std::optional(m_parent->key) : std::nullopt;
  }

private:
  const std::string m_runtime;
  std::optional<Attachment> m_attachment;
  std::optional<CleanupPlan> m_parent, m_terminal;
  bool m_stopping = false, m_retired = false;
};
} // namespace EmuCap::Temporal
