use std::net::IpAddr;

use url::Url;

/// 比较两个 URL 是否属于同一 origin。
pub(crate) fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// 判断主机名字面量是否属于公网地址。
///
/// 这个函数只做字面量判断；带域名的地址还需要解析后的 IP 校验，
/// 否则公网域名仍可能解析到内网。
pub(crate) fn is_public_host(host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".lan")
        || host.ends_with(".home.arpa")
    {
        return false;
    }
    host.parse::<IpAddr>()
        .map(|address| !is_private_or_local_address(address))
        .unwrap_or(true)
}

pub(crate) fn is_private_or_local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_multicast()
                || address.is_broadcast()
                || address.is_documentation()
                || first == 0
                || (first == 100 && (64..=127).contains(&second))
                || (first == 198 && (18..=19).contains(&second))
                || (first == 192 && second == 0 && third == 0)
                || first >= 240
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || (address.segments()[0] == 0x2001 && address.segments()[1] == 0x0db8)
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private_or_local_address(IpAddr::V4(mapped)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_private_or_local_address, is_public_host};
    use std::net::IpAddr;

    #[test]
    fn rejects_private_reserved_and_mapped_addresses() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.1.1",
            "198.18.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            let address = address.parse::<IpAddr>().expect("test address");
            assert!(is_private_or_local_address(address), "{address}");
        }
    }

    #[test]
    fn accepts_public_addresses_and_hosts() {
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let address = address.parse::<IpAddr>().expect("test address");
            assert!(!is_private_or_local_address(address), "{address}");
        }
        for host in ["example.com", "registry-1.docker.io", "1.1.1.1"] {
            assert!(is_public_host(host), "{host}");
        }
    }

    #[test]
    fn rejects_local_host_suffixes() {
        for host in [
            "localhost",
            "service.localhost",
            "service.local",
            "service.internal",
            "service.lan",
            "service.home.arpa",
            "127.0.0.1",
            "100.64.0.1",
        ] {
            assert!(!is_public_host(host), "{host}");
        }
    }
}
