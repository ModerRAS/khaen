#!/bin/bash
echo '== kernel boot lines =='
grep -n 'Linux version' /var/log/kern.log | tail -10
echo '== last reboot =='
last -x reboot shutdown 2>&1 | head -20
echo '== watchdog =='
ls -la /sys/class/watchdog 2>&1
for w in /sys/class/watchdog/watchdog*; do echo "--- $w"; cat "$w/timeout" "$w/state" 2>&1; done
dmesg -T 2>&1 | grep -iE 'watchdog|wdt' | tail -20
echo '== journal reboot hints =='
journalctl -b -1 --no-pager 2>&1 | grep -iE 'reboot|shutdown|watchdog|panic|thermal|hardware' | tail -40
echo '== uptime =='
cat /proc/uptime
echo '== boot id =='
cat /proc/sys/kernel/random/boot_id
