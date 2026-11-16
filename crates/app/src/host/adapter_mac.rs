//! The hardware address this PC announces so viewers can wake it with
//! Wake-on-LAN: that of the network card it reaches the LAN through.

use discovery::{MAC_LEN, MacAddress};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_INET;

/// Windows asks for a bigger buffer when the adapters changed between the
/// size query and the read; a few rounds are plenty.
const MAX_ATTEMPTS: usize = 4;
/// `ERROR_BUFFER_OVERFLOW`: the buffer was too small, `size` says how much.
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const NO_ERROR: u32 = 0;

/// What decides whether a network card is the one to announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Adapter {
    mac: [u8; MAC_LEN],
    /// Connected right now.
    up: bool,
    /// A wired or Wi-Fi card; virtual switches, tunnels and loopback are not.
    physical: bool,
    /// Windows routes the Internet through it: the card of the real LAN,
    /// unlike the virtual switches of Hyper-V or WSL.
    has_gateway: bool,
}

/// The MAC of this PC's main network card, `None` when there is no
/// suitable one or Windows cannot list them: the PC is then still announced,
/// only without a way to wake it.
pub fn primary_mac() -> Option<MacAddress> {
    choose(&list_adapters())
}

/// The first connected physical card that has a gateway, else the first
/// connected physical card.
fn choose(adapters: &[Adapter]) -> Option<MacAddress> {
    let usable = |adapter: &&Adapter| {
        adapter.up && adapter.physical && MacAddress::new(adapter.mac).is_some()
    };
    let mut candidates = adapters.iter().filter(usable);
    let best = candidates.clone().find(|a| a.has_gateway);
    best.or_else(|| candidates.next())
        .and_then(|adapter| MacAddress::new(adapter.mac))
}

fn list_adapters() -> Vec<Adapter> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 0u32;
    // 8-byte aligned storage, as the structures inside need.
    let mut buffer: Vec<u64> = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        let head = if buffer.is_empty() {
            None
        } else {
            Some(buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>())
        };
        // SAFETY: `head` is null or points to a writable buffer of at least
        // `size` bytes, and `size` is a valid output.
        let status =
            unsafe { GetAdaptersAddresses(u32::from(AF_INET.0), flags, None, head, &mut size) };
        match status {
            NO_ERROR if !buffer.is_empty() => {
                // SAFETY: the call succeeded, so the buffer holds a valid chain.
                return unsafe { read_chain(buffer.as_ptr().cast()) };
            }
            ERROR_BUFFER_OVERFLOW | NO_ERROR => {
                buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>()).max(1)];
            }
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

/// # Safety
/// `first` must head a valid chain filled in by `GetAdaptersAddresses` that
/// outlives the call.
unsafe fn read_chain(first: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Adapter> {
    let mut adapters = Vec::new();
    let mut current = first;
    // SAFETY: each node is valid, per the contract; `Next` is null at the end.
    while let Some(node) = unsafe { current.as_ref() } {
        let length = node.PhysicalAddressLength as usize;
        if length == MAC_LEN {
            let mut mac = [0u8; MAC_LEN];
            mac.copy_from_slice(&node.PhysicalAddress[..MAC_LEN]);
            adapters.push(Adapter {
                mac,
                up: node.OperStatus == IfOperStatusUp,
                physical: matches!(node.IfType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211),
                has_gateway: !node.FirstGatewayAddress.is_null(),
            });
        }
        current = node.Next;
    }
    adapters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(last: u8, up: bool, physical: bool, has_gateway: bool) -> Adapter {
        Adapter {
            mac: [0xaa, 0xbb, 0xcc, 0, 0, last],
            up,
            physical,
            has_gateway,
        }
    }

    fn mac(last: u8) -> Option<MacAddress> {
        MacAddress::new([0xaa, 0xbb, 0xcc, 0, 0, last])
    }

    #[test]
    fn the_card_with_the_gateway_wins() {
        let adapters = [
            card(1, true, true, false),
            card(2, true, true, true),
            card(3, true, true, true),
        ];
        assert_eq!(choose(&adapters), mac(2));
    }

    #[test]
    fn without_a_gateway_the_first_connected_physical_card_is_used() {
        let adapters = [
            card(1, false, true, true),
            card(2, true, false, true),
            card(3, true, true, false),
            card(4, true, true, false),
        ];
        assert_eq!(choose(&adapters), mac(3));
    }

    #[test]
    fn disconnected_virtual_or_addressless_cards_are_never_chosen() {
        let unaddressed = Adapter {
            mac: [0; MAC_LEN],
            ..card(0, true, true, true)
        };
        let adapters = [
            card(1, false, true, true),
            card(2, true, false, true),
            unaddressed,
        ];
        assert_eq!(choose(&adapters), None);
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn this_pc_lists_its_adapters_without_failing() {
        // No assumption on the machine: only that reading the list is sound.
        let _ = primary_mac();
    }
}
