// go-signer-check cross-checks the Rust engine's signer against Lighter's official Go
// reference implementation (github.com/elliottech/lighter-go).
//
// Usage:
//
//	cargo run -p lighter-engine --example sign_vector -- <key> <nonce> <expired_at_ms> \
//	  | go run . <key> <nonce> <expired_at_ms>
//
// It (1) rebuilds the same CreateOrder in Go and compares the transaction hash and JSON
// layout, (2) verifies the Rust Schnorr signature with the Go verifier using the Go-derived
// public key, and (3) verifies the Rust auth token the same way.
package main

import (
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"reflect"
	"strconv"
	"strings"

	"github.com/elliottech/lighter-go/client"
	"github.com/elliottech/lighter-go/types"
	g "github.com/elliottech/poseidon_crypto/field/goldilocks"
	p2 "github.com/elliottech/poseidon_crypto/hash/poseidon2_goldilocks"
	schnorr "github.com/elliottech/poseidon_crypto/signature/schnorr"
)

type rustOut struct {
	PublicKey string `json:"public_key"`
	TxType    uint8  `json:"tx_type"`
	TxHash    string `json:"tx_hash"`
	TxInfo    string `json:"tx_info"`
	AuthToken string `json:"auth_token"`
}

func must(err error, what string) {
	if err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %s: %v\n", what, err)
		os.Exit(1)
	}
}

func main() {
	key := "0x07000000000000000300000000000000000000000000000000000000000000000000000000000000"
	nonce := int64(42)
	expiredAt := int64(1_700_000_000_000)
	if len(os.Args) > 1 {
		key = os.Args[1]
	}
	if len(os.Args) > 2 {
		n, err := strconv.ParseInt(os.Args[2], 10, 64)
		must(err, "nonce")
		nonce = n
	}
	if len(os.Args) > 3 {
		e, err := strconv.ParseInt(os.Args[3], 10, 64)
		must(err, "expired_at")
		expiredAt = e
	}

	raw, err := io.ReadAll(os.Stdin)
	must(err, "read stdin")
	var rust rustOut
	must(json.Unmarshal(raw, &rust), "parse rust json")

	c, err := client.NewTxClient(nil, key, 1, 0, 304)
	must(err, "NewTxClient")

	// 1. Same transaction in Go.
	n := nonce
	tx, err := c.GetCreateOrderTransaction(&types.CreateOrderTxReq{
		MarketIndex: 0, ClientOrderIndex: 1, BaseAmount: 1000, Price: 50000, IsAsk: 0,
		Type: 0, TimeInForce: 1, ReduceOnly: 0, TriggerPrice: 0, OrderExpiry: 99_999_999,
	}, &types.TransactOpts{Nonce: &n, ExpiredAt: expiredAt})
	must(err, "GetCreateOrderTransaction")
	goInfo, err := tx.GetTxInfo()
	must(err, "GetTxInfo")

	ok := true
	check := func(name string, cond bool, detail string) {
		if cond {
			fmt.Printf("ok   %s\n", name)
		} else {
			ok = false
			fmt.Printf("FAIL %s: %s\n", name, detail)
		}
	}

	goPub := hex.EncodeToString(func() []byte { b := c.GetKeyManager().PubKeyBytes(); return b[:] }())
	check("public key", strings.EqualFold(strings.TrimPrefix(rust.PublicKey, "0x"), goPub), rust.PublicKey+" vs "+goPub)
	check("tx type", rust.TxType == tx.GetTxType(), fmt.Sprintf("%d vs %d", rust.TxType, tx.GetTxType()))
	check("tx hash", strings.EqualFold(rust.TxHash, tx.GetTxHash()), rust.TxHash+" vs "+tx.GetTxHash())

	var rustMap, goMap map[string]any
	must(json.Unmarshal([]byte(rust.TxInfo), &rustMap), "rust tx_info json")
	must(json.Unmarshal([]byte(goInfo), &goMap), "go tx_info json")
	rustSig, _ := rustMap["Sig"].(string)
	delete(rustMap, "Sig")
	delete(goMap, "Sig")
	check("tx_info fields (excluding Sig)", reflect.DeepEqual(rustMap, goMap), rust.TxInfo+"\n   vs "+goInfo)

	// 2. Rust signature verifies under the Go verifier.
	sigBytes, err := base64.StdEncoding.DecodeString(rustSig)
	must(err, "decode rust sig")
	hashBytes, err := hex.DecodeString(rust.TxHash)
	must(err, "decode tx hash")
	pubBytes, err := hex.DecodeString(strings.TrimPrefix(rust.PublicKey, "0x"))
	must(err, "decode pubkey")
	verr := schnorr.Validate(pubBytes, hashBytes, sigBytes)
	check("rust order signature verifies in Go", verr == nil, fmt.Sprint(verr))

	// Go's own signature must verify too (sanity check of the harness).
	check("go order signature verifies in Go", schnorr.Validate(pubBytes, hashBytes, tx.Sig) == nil, "")

	// 3. Auth token: message = "deadline:account:apiKey", hash = Poseidon2(message), sig hex.
	parts := strings.Split(rust.AuthToken, ":")
	check("auth token has 4 parts", len(parts) == 4, rust.AuthToken)
	if len(parts) == 4 {
		msg := strings.Join(parts[:3], ":")
		elems, err := g.ArrayFromCanonicalLittleEndianBytes([]byte(msg))
		must(err, "auth msg to field")
		msgHash := p2.HashToQuinticExtension(elems).ToLittleEndianBytes()
		tokSig, err := hex.DecodeString(parts[3])
		must(err, "auth sig hex")
		check("rust auth token verifies in Go", schnorr.Validate(pubBytes, msgHash, tokSig) == nil, "")
	}

	if !ok {
		os.Exit(1)
	}
	fmt.Println("ALL CHECKS PASSED")
}
