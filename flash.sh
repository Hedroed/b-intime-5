#!/bin/bash
espflash flash --monitor --log-format defmt --chip esp32c6 --elf "$1" "$1"
