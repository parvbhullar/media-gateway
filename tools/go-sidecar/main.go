package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"path/filepath"
	"regexp"
	"strconv"
	"syscall"
	"time"

	"github.com/joho/godotenv"
	"github.com/livekit/protocol/auth"
)

var reUnsafe = regexp.MustCompile(`[^A-Za-z0-9._-]`)

func roomNameFor(callID, did string) string {
	base := callID
	if base == "" {
		base = "sip-" + did
	}
	safe := reUnsafe.ReplaceAllString(base, "-")
	return "sip-" + safe
}

func buildSIPAttributes(callID, did, caller string) map[string]string {
	attrs := map[string]string{
		"sip.callID":     callID,
		"sip.callStatus": "active",
	}
	if caller != "" {
		attrs["sip.phoneNumber"] = caller
	}
	if did != "" {
		attrs["sip.trunkPhoneNumber"] = did
	}
	raw := os.Getenv("SIP_SIDECAR_HEADERS")
	if raw != "" {
		var headers map[string]string
		if err := json.Unmarshal([]byte(raw), &headers); err != nil {
			slog.Warn("ignoring malformed SIP_SIDECAR_HEADERS", "err", err)
		} else {
			for k, v := range headers {
				attrs["sip.h."+k] = v
			}
		}
	}
	return attrs
}

func buildToken(apiKey, apiSecret, roomName, caller string, attrs map[string]string) (string, error) {
	identity := fmt.Sprintf("sip-caller-%s", caller)
	at := auth.NewAccessToken(apiKey, apiSecret)
	grant := &auth.VideoGrant{
		RoomJoin: true,
		Room:     roomName,
	}
	at.AddGrant(grant).
		SetIdentity(identity).
		SetName(identity).
		SetAttributes(attrs)
	// To mark the participant as kind="sip", call at.SetKind(livekit.ParticipantInfo_SIP)
	// if your server-sdk-go version exposes it on AccessToken.
	return at.ToJWT()
}

// loadDotenv mirrors Python's search order — first hit wins, existing env vars
// are never overwritten (godotenv.Load semantics):
//  1. $SIP_SIDECAR_ENV if set and file exists
//  2. <executable dir>/.env
//  3. Walk up from os.Getwd() until .env found or filesystem root
func loadDotenv() {
	if p := os.Getenv("SIP_SIDECAR_ENV"); p != "" {
		if _, err := os.Stat(p); err == nil {
			_ = godotenv.Load(p)
			return
		}
	}
	if exe, err := os.Executable(); err == nil {
		p := filepath.Join(filepath.Dir(exe), ".env")
		if _, err := os.Stat(p); err == nil {
			_ = godotenv.Load(p)
			return
		}
	}
	dir, _ := os.Getwd()
	for {
		p := filepath.Join(dir, ".env")
		if _, err := os.Stat(p); err == nil {
			_ = godotenv.Load(p)
			return
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
}

func agentJoinTimeoutDuration() time.Duration {
	if s := os.Getenv("AGENT_JOIN_TIMEOUT"); s != "" {
		if n, err := strconv.ParseFloat(s, 64); err == nil && n > 0 {
			return time.Duration(n * float64(time.Second))
		}
	}
	return 30 * time.Second
}

func mustEnv(key string) string {
	v := os.Getenv(key)
	if v == "" {
		fmt.Fprintf(os.Stderr, "required env var %s is not set\n", key)
		os.Exit(1)
	}
	return v
}

func envOrDefault(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func main() {
	var callID, did, caller string
	var port int
	flag.StringVar(&callID, "call-id", "", "SIP Call-ID (required)")
	flag.StringVar(&did, "did", "", "dialed DID / to-user (required)")
	flag.StringVar(&caller, "caller", "", "caller number / from-user (required)")
	flag.IntVar(&port, "port", 0, "rustpbx PCM UDP port (required)")
	flag.Parse()

	if callID == "" || did == "" || caller == "" || port == 0 {
		fmt.Fprintln(os.Stderr,
			"usage: sip_bridge_sidecar --call-id=<id> --did=<did> --caller=<from> --port=<port>")
		os.Exit(1)
	}

	loadDotenv()

	cfg := Config{
		CallID:           callID,
		DID:              did,
		Caller:           caller,
		RustpbxPort:      port,
		LiveKitURL:       mustEnv("LIVEKIT_URL"),
		APIKey:           mustEnv("LIVEKIT_API_KEY"),
		APISecret:        mustEnv("LIVEKIT_API_SECRET"),
		AgentName:        envOrDefault("AGENT_NAME", "Aria"),
		AgentJoinTimeout: agentJoinTimeoutDuration(),
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, os.Interrupt)
	defer stop()

	if err := Run(ctx, cfg); err != nil {
		slog.Error("bridge failed", "err", err)
		os.Exit(1)
	}
}
