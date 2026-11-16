//! The request of a viewer to be accepted, as the window shows it: who
//! asks, and whether to remember its device. Pure state; the host decides
//! when the request ends (see `HostEvent::ApprovalClosed`).

use std::net::SocketAddr;

use crate::host::{ApprovalAnswer, ApprovalId, ApprovalRequest};
use crate::text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub id: ApprovalId,
    pub viewer_name: String,
    pub address: SocketAddr,
    /// The "Ricorda questo dispositivo" box.
    pub remember: bool,
}

impl ApprovalPrompt {
    /// Remembering is on until the user turns it off: the usual wish is
    /// not to be asked again by their own laptop.
    pub fn new(request: ApprovalRequest) -> Self {
        Self {
            id: request.id,
            viewer_name: request.viewer_name,
            address: request.address,
            remember: true,
        }
    }

    /// "<name> (<ip>) vuole connettersi".
    pub fn message(&self) -> String {
        text::approval_message(&self.viewer_name, self.address.ip())
    }

    /// What the click means to the host. Accepting is only ever the result
    /// of `accept: true`.
    pub fn answer(&self, accept: bool) -> ApprovalAnswer {
        if accept {
            ApprovalAnswer::Accept {
                remember: self.remember,
            }
        } else {
            ApprovalAnswer::Refuse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> ApprovalPrompt {
        ApprovalPrompt::new(ApprovalRequest {
            id: 7,
            viewer_name: "PORTATILE".into(),
            address: "192.168.1.7:5000".parse().unwrap(),
        })
    }

    #[test]
    fn remembering_the_device_is_on_by_default() {
        assert!(prompt().remember);
    }

    #[test]
    fn the_message_names_the_viewer_and_its_address_without_the_port() {
        assert_eq!(
            prompt().message(),
            "PORTATILE (192.168.1.7) vuole connettersi"
        );
    }

    #[test]
    fn the_answer_follows_the_click_and_the_remember_box() {
        let mut prompt = prompt();
        assert_eq!(
            prompt.answer(true),
            ApprovalAnswer::Accept { remember: true }
        );
        prompt.remember = false;
        assert_eq!(
            prompt.answer(true),
            ApprovalAnswer::Accept { remember: false }
        );
        assert_eq!(prompt.answer(false), ApprovalAnswer::Refuse);
    }
}
