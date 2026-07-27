package main

import (
	"os"
	"testing"
)

func TestRoomNameFor(t *testing.T) {
	cases := []struct {
		callID, did, want string
	}{
		{"abc123", "1000", "sip-abc123"},
		{"", "1000", "sip-sip-1000"},
		{"-jLtLvcXCy", "1000", "sip--jLtLvcXCy"},
		{"abc def/ghi", "1000", "sip-abc-def-ghi"},
		{"abc@192.168.1.1", "1000", "sip-abc-192.168.1.1"},
	}
	for _, c := range cases {
		got := roomNameFor(c.callID, c.did)
		if got != c.want {
			t.Errorf("roomNameFor(%q, %q) = %q, want %q", c.callID, c.did, got, c.want)
		}
	}
}

func TestBuildSIPAttributes_Basic(t *testing.T) {
	os.Unsetenv("SIP_SIDECAR_HEADERS")
	attrs := buildSIPAttributes("call-1", "15551234567", "15559876543")

	check := func(key, want string) {
		t.Helper()
		if got := attrs[key]; got != want {
			t.Errorf("attrs[%q] = %q, want %q", key, got, want)
		}
	}
	check("sip.callID", "call-1")
	check("sip.callStatus", "active")
	check("sip.phoneNumber", "15559876543")
	check("sip.trunkPhoneNumber", "15551234567")
}

func TestBuildSIPAttributes_XHeaders(t *testing.T) {
	os.Setenv("SIP_SIDECAR_HEADERS", `{"X-Tenant":"acme","X-Lang":"en"}`)
	defer os.Unsetenv("SIP_SIDECAR_HEADERS")

	attrs := buildSIPAttributes("c", "d", "e")
	if attrs["sip.h.X-Tenant"] != "acme" {
		t.Errorf("X-Tenant not mapped: %v", attrs)
	}
	if attrs["sip.h.X-Lang"] != "en" {
		t.Errorf("X-Lang not mapped: %v", attrs)
	}
}

func TestBuildSIPAttributes_MalformedHeaders(t *testing.T) {
	os.Setenv("SIP_SIDECAR_HEADERS", `not-json`)
	defer os.Unsetenv("SIP_SIDECAR_HEADERS")

	attrs := buildSIPAttributes("c", "d", "e")
	for k := range attrs {
		if len(k) > 6 && k[:6] == "sip.h." {
			t.Errorf("unexpected sip.h.* key with bad JSON: %q", k)
		}
	}
}

func TestAgentJoinTimeoutDefault(t *testing.T) {
	os.Unsetenv("AGENT_JOIN_TIMEOUT")
	d := agentJoinTimeoutDuration()
	if d.Seconds() != 30 {
		t.Errorf("want 30s default, got %s", d)
	}
}

func TestAgentJoinTimeoutEnv(t *testing.T) {
	os.Setenv("AGENT_JOIN_TIMEOUT", "45")
	defer os.Unsetenv("AGENT_JOIN_TIMEOUT")
	d := agentJoinTimeoutDuration()
	if d.Seconds() != 45 {
		t.Errorf("want 45s, got %s", d)
	}
}
