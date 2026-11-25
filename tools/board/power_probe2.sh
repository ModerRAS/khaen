#!/bin/bash
echo '== soak =='
systemctl is-active khaen-soak12.service 2>&1
tail -2 /var/lib/khaen-soak/status.csv 2>&1
echo '== regulators =='
for r in /sys/class/regulator/*; do
  [ -e "$r/name" ] || continue
  n=$(cat "$r/name" 2>/dev/null)
  v=$(cat "$r/microvolts" 2>/dev/null)
  echo "$n $v"
done | sort
echo '== usb bMaxPower =='
for d in /sys/bus/usb/devices/*; do
  [ -e "$d/bMaxPower" ] || continue
  echo "$(basename "$d") $(cat "$d/idVendor" 2>/dev/null):$(cat "$d/idProduct" 2>/dev/null) $(cat "$d/product" 2>/dev/null) max=$(cat "$d/bMaxPower" 2>/dev/null)"
done
echo '== lsusb tree =='
command -v lsusb >/dev/null && lsusb -t 2>&1 || echo '(no lsusb)'
echo '== dmesg power hints =='
dmesg -T 2>&1 | grep -iE 'over-current|overcurrent|undervolt|brownout|vbus|voltage' | tail -30
echo '== power_supply =='
for p in /sys/class/power_supply/*; do [ -e "$p" ] && echo "--- $p" && cat "$p/uevent" 2>/dev/null; done
