#!/bin/bash
echo '== pstore =='
ls -la /sys/fs/pstore 2>&1
for f in /sys/fs/pstore/*; do
  echo "--- $f"
  strings "$f" 2>/dev/null | tail -60
done
echo '== prev tail =='
journalctl -b -1 --no-pager 2>&1 | tail -35
echo '== logs =='
ls -la /var/log 2>&1 | head -30
for f in /var/log/syslog /var/log/kern.log /var/log/messages; do
  echo "--- $f"
  tail -15 "$f" 2>&1
done
echo '== last csv =='
cat /var/lib/tspi-soak/status.csv
echo '== dmesg pstore =='
dmesg -T 2>&1 | grep -iE 'pstore|ramoops|panic|oops|reset' | tail -30
