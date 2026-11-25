#!/bin/bash
echo '== soak dir =='
ls -la /var/lib/tspi-soak 2>&1
echo '== soak.log =='
cat /var/lib/tspi-soak/soak.log 2>&1
echo '== units =='
systemctl list-units --all --no-pager 2>&1 | grep -i soak
ls -la /etc/systemd/system 2>&1 | grep -i soak
echo '== candidate scripts =='
find /usr/local/bin /opt /root /home -maxdepth 3 -iname '*soak*' -o -iname '*tspi*' 2>/dev/null | head -60
echo '== modules =='
lsmod | grep -E 'snd_aloop|snd_usb_audio|merging'
echo '== cards =='
cat /proc/asound/cards
echo '== camilla =='
command -v camilladsp; camilladsp --version 2>&1
ls -la /etc/tspi-audio /var/lib/tspi-audio 2>&1
