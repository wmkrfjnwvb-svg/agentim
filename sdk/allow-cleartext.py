"""Lets a test app talk plain HTTP to the test server on the build machine
(Android): sets android:usesCleartextTraffic="true" on <application>,
replacing a placeholder React Native puts there. Adds the INTERNET
permission when missing (Flutter adds it only to debug builds).

    python3 sdk/allow-cleartext.py <AndroidManifest.xml>
"""
import re
import sys

p = sys.argv[1]
s = open(p).read()
if "usesCleartextTraffic" in s:
    s = re.sub(r'android:usesCleartextTraffic="[^"]*"', 'android:usesCleartextTraffic="true"', s)
else:
    s = s.replace("<application", '<application android:usesCleartextTraffic="true"', 1)
if "android.permission.INTERNET" not in s:
    s = re.sub(r"(<manifest[^>]*>)", r'\1\n    <uses-permission android:name="android.permission.INTERNET" />', s, count=1)
open(p, "w").write(s)
