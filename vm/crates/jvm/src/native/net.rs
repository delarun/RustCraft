//! java.net natives: name resolution and network interfaces. IPv4 only; the
//! single interface is loopback. Natively, host names resolve through the OS;
//! the browser build will only know `localhost`.

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;

pub(super) fn register(n: &mut Natives) {
    n.add("java/net/InetAddress.init()V", no_op);
    n.add("java/net/InetAddress.isIPv4Available()Z", |_, _| ret_z(true));
    n.add("java/net/InetAddress.isIPv6Supported()Z", return_false);
    n.add("java/net/Inet4Address.init()V", no_op);
    n.add("java/net/Inet6Address.init()V", no_op);
    n.add("java/net/NetworkInterface.init()V", no_op);

    n.add("java/net/Inet4AddressImpl.getLocalHostName()Ljava/lang/String;", |t, _| ret_ref(t.new_string("localhost")?));
    n.add("java/net/Inet4AddressImpl.lookupAllHostAddr(Ljava/lang/String;)[Ljava/net/InetAddress;", |t, a| {
        let host = t.vm.string_value(t.null_check(arg_ref(a, 1))?);
        let addresses = resolve(&host);
        if addresses.is_empty() {
            return Err(t.throw_new("java/net/UnknownHostException", Some(&format!("{host}: Name or service not known"))));
        }
        let mut items = Vec::with_capacity(addresses.len());
        for address in addresses {
            items.push(inet4(t, &host, address)?);
        }
        ret_ref(t.new_object_array("java/net/InetAddress", &items)?)
    });
    n.add("java/net/Inet4AddressImpl.getHostByAddr([B)Ljava/lang/String;", |t, a| {
        let bytes = t.null_check(arg_ref(a, 1))?;
        let raw = t.vm.heap.bytes(bytes, ARRAY_BASE_OFFSET, 4);
        if raw == [127, 0, 0, 1] {
            return ret_ref(t.new_string("localhost")?);
        }
        Err(t.throw_new("java/net/UnknownHostException", None))
    });
    n.add("java/net/Inet4AddressImpl.isReachable0([BI[BI)Z", return_false);

    n.add("java/net/NetworkInterface.getAll()[Ljava/net/NetworkInterface;", |t, _| {
        let lo = loopback(t)?;
        ret_ref(t.new_object_array("java/net/NetworkInterface", &[lo])?)
    });
    n.add("java/net/NetworkInterface.getByName0(Ljava/lang/String;)Ljava/net/NetworkInterface;", |t, a| {
        let name = t.vm.string_value(t.null_check(arg_ref(a, 0))?);
        ret_ref(if name == "lo" { loopback(t)? } else { Ref::NULL })
    });
    n.add("java/net/NetworkInterface.getByIndex0(I)Ljava/net/NetworkInterface;", |t, a| {
        ret_ref(if arg_i(a, 0) == 1 { loopback(t)? } else { Ref::NULL })
    });
    n.add("java/net/NetworkInterface.getByInetAddress0(Ljava/net/InetAddress;)Ljava/net/NetworkInterface;", |t, a| {
        let address = t.null_check(arg_ref(a, 0))?;
        ret_ref(if is_loopback(t, address) { loopback(t)? } else { Ref::NULL })
    });
    n.add("java/net/NetworkInterface.boundInetAddress0(Ljava/net/InetAddress;)Z", |t, a| {
        let address = t.null_check(arg_ref(a, 0))?;
        ret_z(is_loopback(t, address))
    });
    n.add("java/net/NetworkInterface.isUp0(Ljava/lang/String;I)Z", |_, _| ret_z(true));
    n.add("java/net/NetworkInterface.isLoopback0(Ljava/lang/String;I)Z", |_, _| ret_z(true));
    n.add("java/net/NetworkInterface.supportsMulticast0(Ljava/lang/String;I)Z", return_false);
    n.add("java/net/NetworkInterface.isP2P0(Ljava/lang/String;I)Z", return_false);
    n.add("java/net/NetworkInterface.getMacAddr0([BLjava/lang/String;I)[B", return_null);
    n.add("java/net/NetworkInterface.getMTU0(Ljava/lang/String;I)I", |_, _| ret_i(65536));
}

/// IPv4 addresses for a host name, as big-endian integers.
fn resolve(host: &str) -> Vec<u32> {
    if host.is_empty() || host == "localhost" {
        return vec![0x7F00_0001];
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return vec![u32::from(ip)];
    }
    #[cfg(not(target_family = "wasm"))]
    {
        use std::net::ToSocketAddrs;
        if let Ok(addresses) = (host, 0).to_socket_addrs() {
            return addresses
                .filter_map(|a| match a.ip() {
                    std::net::IpAddr::V4(ip) => Some(u32::from(ip)),
                    std::net::IpAddr::V6(_) => None,
                })
                .collect();
        }
    }
    Vec::new()
}

pub(crate) fn inet4(t: &mut Thread<'_>, host: &str, address: u32) -> JResult<Ref> {
    let class = t.load_class("java/net/Inet4Address")?;
    t.ensure_initialized(class)?;
    let obj = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    let host = t.new_string(host)?;
    let init = class.find_method("<init>", "(Ljava/lang/String;I)V").expect("Inet4Address(String, int)");
    t.invoke(init, &[obj.slot(), host.slot(), address as u64])?;
    Ok(obj)
}

fn loopback(t: &mut Thread<'_>) -> JResult<Ref> {
    let address = inet4(t, "localhost", 0x7F00_0001)?;
    let addresses = t.new_object_array("java/net/InetAddress", &[address])?;
    let class = t.load_class("java/net/NetworkInterface")?;
    t.ensure_initialized(class)?;
    let obj = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    let name = t.new_string("lo")?;
    let init = class.find_method("<init>", "(Ljava/lang/String;I[Ljava/net/InetAddress;)V").expect("NetworkInterface ctor");
    t.invoke(init, &[obj.slot(), name.slot(), 1, addresses.slot()])?;
    Ok(obj)
}

fn is_loopback(t: &mut Thread<'_>, address: Ref) -> bool {
    let holder = t.field_ref(address, "java/net/InetAddress", "holder");
    !holder.is_null() && t.field(holder, "java/net/InetAddress$InetAddressHolder", "address") as u32 >> 24 == 127
}
