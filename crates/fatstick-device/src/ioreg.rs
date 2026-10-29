//! macOS: USB serial numbers from the I/O Registry (`ioreg -a`), by the BSD
//! name of each disk below a USB device ("disk4"). DiskArbitration doesn't
//! give a disk's serial number; the USB device it hangs off does.

use std::collections::HashMap;
use std::io;

/// The serial of the nearest USB device above each disk, from `ioreg -a -l`
/// output (an array of registry subtrees as a property list). A USB device
/// is a node with a `bDeviceClass` (its interfaces have `bInterfaceClass`)
/// or of class `IOUSBHostDevice`; a hub's serial isn't passed down to a
/// device below it that has none.
pub fn serials_by_disk(bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(v) = plist::Value::from_reader(io::Cursor::new(bytes)) {
        walk(&v, "", &mut out);
    }
    out
}

fn walk(v: &plist::Value, serial: &str, out: &mut HashMap<String, String>) {
    match v {
        plist::Value::Array(a) => {
            for x in a {
                walk(x, serial, out);
            }
        }
        plist::Value::Dictionary(d) => {
            let text = |k: &str| d.get(k).and_then(|x| x.as_string()).unwrap_or("").trim().to_string();
            let device = d.contains_key("bDeviceClass") || text("IOObjectClass") == "IOUSBHostDevice";
            let own = if device {
                let s = text("USB Serial Number");
                if s.is_empty() {
                    text("kUSBSerialNumberString")
                } else {
                    s
                }
            } else {
                serial.to_string()
            };
            let bsd = text("BSD Name");
            if !bsd.is_empty() && !own.is_empty() {
                out.entry(bsd).or_insert_with(|| own.clone());
            }
            if let Some(kids) = d.get("IORegistryEntryChildren") {
                walk(kids, &own, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(class: &str, props: &str, kids: &[String]) -> String {
        let kids = if kids.is_empty() { String::new() } else { format!("<key>IORegistryEntryChildren</key><array>{}</array>", kids.join("")) };
        format!("<dict><key>IOObjectClass</key><string>{class}</string>{props}{kids}</dict>")
    }
    fn media(bsd: &str) -> String {
        node("IOMedia", &format!("<key>BSD Name</key><string>{bsd}</string>"), &[node("IOMedia", &format!("<key>BSD Name</key><string>{bsd}s1</string>"), &[])])
    }

    #[test]
    fn each_disk_gets_the_serial_of_the_usb_device_it_hangs_off() {
        // A hub with a serial; below it a stick with one, and a card reader without.
        let stick = node(
            "IOUSBHostDevice",
            "<key>bDeviceClass</key><integer>0</integer><key>USB Serial Number</key><string> 4C530001230512345678 </string>",
            &[node("IOUSBHostInterface", "<key>bInterfaceClass</key><integer>8</integer>", &[node("IOSCSIPeripheralDeviceType00", "", &[node("IOBlockStorageDriver", "", &[media("disk4")])])])],
        );
        let reader = node(
            "IOUSBHostDevice",
            "<key>bDeviceClass</key><integer>0</integer>",
            &[node("IOUSBHostInterface", "<key>bInterfaceClass</key><integer>8</integer>", &[media("disk5")])],
        );
        let old = node("AppleUSBDevice", "<key>bDeviceClass</key><integer>0</integer><key>kUSBSerialNumberString</key><string>OLD1</string>", &[media("disk6")]);
        let hub = node("IOUSBHostDevice", "<key>bDeviceClass</key><integer>9</integer><key>USB Serial Number</key><string>HUB</string>", &[stick.clone(), reader, old]);
        // ioreg -r lists the hub's subtree, and the stick's again on its own.
        let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><array>{hub}{stick}</array></plist>"#);
        let m = serials_by_disk(xml.as_bytes());
        assert_eq!(m.get("disk4").map(String::as_str), Some("4C530001230512345678"));
        assert_eq!(m.get("disk4s1").map(String::as_str), Some("4C530001230512345678"));
        assert_eq!(m.get("disk5"), None, "not the hub's serial");
        assert_eq!(m.get("disk6").map(String::as_str), Some("OLD1"));
        assert!(serials_by_disk(b"not a plist").is_empty());
    }
}
