// Copyright 2022 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use accesskit::{Live, NodeId};
use accesskit_consumer::{DetachedNode, FilterResult, Node, TreeChangeHandler, TreeState};
use objc2::{
    foundation::{NSInteger, NSMutableDictionary, NSNumber, NSObject, NSString},
    msg_send, Message,
};
use std::rc::Rc;

use crate::{
    appkit::*,
    context::Context,
    node::{filter, filter_detached, NodeWrapper},
};

// Workaround for https://github.com/madsmtm/objc2/issues/306
fn set_object_for_key<K: Message, V: Message>(
    dictionary: &mut NSMutableDictionary<K, V>,
    value: &V,
    key: &K,
) {
    let _: () = unsafe { msg_send![dictionary, setObject: value, forKey: key] };
}

// This type is designed to be safe to create on a non-main thread
// and send to the main thread. This ability isn't yet used though.
pub(crate) enum QueuedEvent {
    Generic {
        node_id: NodeId,
        notification: &'static NSString,
    },
    NodeDestroyed(NodeId),
    Announcement {
        text: String,
        priority: NSInteger,
    },
}

impl QueuedEvent {
    fn live_region_announcement(node: &Node) -> Self {
        Self::Announcement {
            text: node.name().unwrap(),
            priority: if node.live() == Live::Assertive {
                NSAccessibilityPriorityHigh
            } else {
                NSAccessibilityPriorityMedium
            },
        }
    }

    fn raise(self, context: &Rc<Context>) {
        match self {
            Self::Generic {
                node_id,
                notification,
            } => {
                let platform_node = context.get_or_create_platform_node(node_id);
                unsafe { NSAccessibilityPostNotification(&platform_node, notification) };
            }
            Self::NodeDestroyed(node_id) => {
                if let Some(platform_node) = context.remove_platform_node(node_id) {
                    unsafe {
                        NSAccessibilityPostNotification(
                            &platform_node,
                            NSAccessibilityUIElementDestroyedNotification,
                        )
                    };
                }
            }
            Self::Announcement { text, priority } => {
                let view = match context.view.load() {
                    Some(view) => view,
                    None => {
                        return;
                    }
                };

                let window = match view.window() {
                    Some(window) => window,
                    None => {
                        return;
                    }
                };

                let mut user_info = NSMutableDictionary::<_, NSObject>::new();
                let text = NSString::from_str(&text);
                set_object_for_key(&mut user_info, &*text, unsafe {
                    NSAccessibilityAnnouncementKey
                });
                let priority = NSNumber::new_isize(priority);
                set_object_for_key(&mut user_info, &*priority, unsafe {
                    NSAccessibilityPriorityKey
                });

                unsafe {
                    NSAccessibilityPostNotificationWithUserInfo(
                        &window,
                        NSAccessibilityAnnouncementRequestedNotification,
                        &user_info,
                    )
                };
            }
        }
    }
}

/// Events generated by a tree update.
#[must_use = "events must be explicitly raised"]
pub struct QueuedEvents {
    context: Rc<Context>,
    events: Vec<QueuedEvent>,
}

impl QueuedEvents {
    /// Raise all queued events synchronously.
    ///
    /// It is unknown whether accessibility methods on the view may be
    /// called while events are being raised. This means that any locks
    /// or runtime borrows required to access the adapter must not
    /// be held while this method is called.
    pub fn raise(self) {
        for event in self.events {
            event.raise(&self.context);
        }
    }
}

pub(crate) struct EventGenerator {
    context: Rc<Context>,
    events: Vec<QueuedEvent>,
}

impl EventGenerator {
    pub(crate) fn new(context: Rc<Context>) -> Self {
        Self {
            context,
            events: Vec::new(),
        }
    }

    pub(crate) fn into_result(self) -> QueuedEvents {
        QueuedEvents {
            context: self.context,
            events: self.events,
        }
    }
}

impl TreeChangeHandler for EventGenerator {
    fn node_added(&mut self, node: &Node) {
        if filter(node) != FilterResult::Include {
            return;
        }
        if node.name().is_some() && node.live() != Live::Off {
            self.events
                .push(QueuedEvent::live_region_announcement(node));
        }
    }

    fn node_updated(&mut self, old_node: &DetachedNode, new_node: &Node) {
        // TODO: text changes, live regions
        if filter(new_node) != FilterResult::Include {
            return;
        }
        let node_id = new_node.id();
        let old_wrapper = NodeWrapper::DetachedNode(old_node);
        let new_wrapper = NodeWrapper::Node(new_node);
        if old_wrapper.title() != new_wrapper.title() {
            self.events.push(QueuedEvent::Generic {
                node_id,
                notification: unsafe { NSAccessibilityTitleChangedNotification },
            });
        }
        if old_wrapper.value() != new_wrapper.value() {
            self.events.push(QueuedEvent::Generic {
                node_id,
                notification: unsafe { NSAccessibilityValueChangedNotification },
            });
        }
        if old_wrapper.supports_text_ranges()
            && new_wrapper.supports_text_ranges()
            && old_wrapper.raw_text_selection() != new_wrapper.raw_text_selection()
        {
            self.events.push(QueuedEvent::Generic {
                node_id,
                notification: unsafe { NSAccessibilitySelectedTextChangedNotification },
            });
        }
        if new_node.name().is_some()
            && new_node.live() != Live::Off
            && (new_node.name() != old_node.name()
                || new_node.live() != old_node.live()
                || filter_detached(old_node) != FilterResult::Include)
        {
            self.events
                .push(QueuedEvent::live_region_announcement(new_node));
        }
    }

    fn focus_moved(&mut self, _old_node: Option<&DetachedNode>, new_node: Option<&Node>) {
        if let Some(new_node) = new_node {
            if filter(new_node) != FilterResult::Include {
                return;
            }
            self.events.push(QueuedEvent::Generic {
                node_id: new_node.id(),
                notification: unsafe { NSAccessibilityFocusedUIElementChangedNotification },
            });
        }
    }

    fn node_removed(&mut self, node: &DetachedNode, _current_state: &TreeState) {
        self.events.push(QueuedEvent::NodeDestroyed(node.id()));
    }
}