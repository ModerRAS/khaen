#!/bin/bash
echo '== xrun rows =='
awk -F, 'NR==1 || $4+0>0 {print NR": "$0}' /var/lib/khaen-soak/status.csv
echo '== camilla errors =='
grep -inE 'underrun|overrun|XRUN|Broken pipe|error' /var/lib/khaen-soak/camilla.log | tail -20
echo '== camilla size/tail =='
ls -l /var/lib/khaen-soak/camilla.log; tail -5 /var/lib/khaen-soak/camilla.log
