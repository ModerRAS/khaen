#!/bin/bash
echo '== xrun rows =='
awk -F, 'NR==1 || $4+0>0 {print NR": "$0}' /var/lib/tspi-soak/status.csv
echo '== camilla errors =='
grep -inE 'underrun|overrun|XRUN|Broken pipe|error' /var/lib/tspi-soak/camilla.log | tail -20
echo '== camilla size/tail =='
ls -l /var/lib/tspi-soak/camilla.log; tail -5 /var/lib/tspi-soak/camilla.log
