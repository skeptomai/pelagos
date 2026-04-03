#!/bin/bash
set -e

# Initialise the image store and pelagos group.
/usr/share/pelagos/setup.sh --no-user

echo ""
echo "==> pelagos installed."
echo ""
echo "    Add yourself to the pelagos group to pull images without sudo:"
echo "      sudo usermod -aG pelagos \$USER"
echo "    Then log out and back in (or run 'newgrp pelagos' in this shell)."
echo ""
echo "    Quick start:"
echo "      pelagos image pull alpine"
echo "      pelagos run alpine /bin/echo hello"
