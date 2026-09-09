// Command gofmt-probe records two Go formatting behaviors that the Rust port of
// wire-pod must reproduce byte for byte:
//
//  1. fmt.Sprintf("%v", x) for float32 values, as used by the Go server's
//     get_stim_status handler, which passes stimState's float32 to fmt.Fprint.
//  2. encoding/json marshaling of float64 values, as used by net_probe's rttMs
//     field.
//
// Output format is one case per line: <section>\t<source literal>\t<output>.
// Sections are "f32v" and "f64json".
//
// expected.txt in this directory is the recorded stdout of `go run main.go` and
// is consumed as table-driven test data by the Rust tests in wirepod-core.
// Regenerate it with:
//
//	go run main.go > expected.txt
package main

import (
	"encoding/json"
	"fmt"
	"math"
)

type f32Case struct {
	lit string
	val float32
}

type f64Case struct {
	lit string
	val float64
}

func main() {
	f32Cases := []f32Case{
		{"float32(0)", float32(0)},
		{"float32(math.Copysign(0, -1))", float32(math.Copysign(0, -1))},
		{"float32(1)", float32(1)},
		{"float32(0.1)", float32(0.1)},
		{"float32(0.75)", float32(0.75)},
		{"float32(0.5325)", float32(0.5325)},
		{"float32(5e-05)", float32(5e-05)},
		{"float32(0.0001)", float32(0.0001)},
		{"float32(0.001)", float32(0.001)},
		{"float32(1.5)", float32(1.5)},
		{"float32(2.5)", float32(2.5)},
		{"float32(-0.25)", float32(-0.25)},
		{"float32(100000)", float32(100000)},
		{"float32(999999)", float32(999999)},
		{"float32(1e6)", float32(1e6)},
		{"float32(1.5e6)", float32(1.5e6)},
		{"float32(1234567)", float32(1234567)},
		{"float32(123456789)", float32(123456789)},
		{"float32(1e20)", float32(1e20)},
		{"float32(1e21)", float32(1e21)},
		{"float32(1e22)", float32(1e22)},
		{"float32(math.MaxFloat32)", float32(math.MaxFloat32)},
		{"float32(math.SmallestNonzeroFloat32)", float32(math.SmallestNonzeroFloat32)},
		{"float32(math.Inf(1))", float32(math.Inf(1))},
		{"float32(math.Inf(-1))", float32(math.Inf(-1))},
		{"float32(math.NaN())", float32(math.NaN())},
		// Exact halfway ties. Two digit strings of the same shortest length
		// both round-trip, and strconv picks the one whose last digit is even
		// (ftoaryu.go, ryuDigits32). Rust's shortest formatter picks the larger
		// one instead, so these four are the cases that catch a port that took
		// Rust's answer unchanged. They are written as bit patterns because a
		// decimal literal for them would beg the question.
		{"math.Float32frombits(0x3ee90000)", math.Float32frombits(0x3ee90000)},
		{"math.Float32frombits(0x4a2e4051)", math.Float32frombits(0x4a2e4051)},
		{"math.Float32frombits(0xca2e4051)", math.Float32frombits(0xca2e4051)},
		{"math.Float32frombits(0x4a24586d)", math.Float32frombits(0x4a24586d)},
	}
	for _, c := range f32Cases {
		fmt.Printf("f32v\t%s\t%v\n", c.lit, c.val)
	}

	f64Cases := []f64Case{
		{"float64(0)", float64(0)},
		{"float64(math.Copysign(0, -1))", math.Copysign(0, -1)},
		{"float64(13)", float64(13)},
		{"float64(13.482)", float64(13.482)},
		{"float64(14.0)", float64(14.0)},
		{"float64(0.1)", float64(0.1)},
		{"float64(-0.5)", float64(-0.5)},
		{"float64(0.000001)", float64(0.000001)},
		{"float64(1e-6)", float64(1e-6)},
		{"float64(1e-7)", float64(1e-7)},
		{"float64(1e17)", float64(1e17)},
		{"float64(1e20)", float64(1e20)},
		{"float64(1e21)", float64(1e21)},
		{"float64(123456789012345678)", float64(123456789012345678)},
		{"float64(1.5e300)", float64(1.5e300)},
		{"float64(5e-324)", float64(5e-324)},
		{"float64(math.MaxFloat64)", math.MaxFloat64},
		{"float64(1.0000000000000002)", float64(1.0000000000000002)},
		// The same halfway ties one width up. encoding/json goes through the
		// same strconv shortest formatter, so the round-to-even rule reaches
		// rttMs too.
		{"math.Float64frombits(0x42ba3969c09532d0)", math.Float64frombits(0x42ba3969c09532d0)},
		{"math.Float64frombits(0x42e2d93924dc6844)", math.Float64frombits(0x42e2d93924dc6844)},
		{"math.Float64frombits(0xc2ee0c6f8a839d14)", math.Float64frombits(0xc2ee0c6f8a839d14)},
	}
	for _, c := range f64Cases {
		b, err := json.Marshal(c.val)
		if err != nil {
			fmt.Printf("f64json\t%s\tERROR: %s\n", c.lit, err.Error())
			continue
		}
		fmt.Printf("f64json\t%s\t%s\n", c.lit, string(b))
	}

	// json.Marshal rejects NaN and the infinities; record the error text so the
	// Rust side knows what Go does instead of guessing.
	for _, c := range []f64Case{
		{"NaN", math.NaN()},
		{"+Inf", math.Inf(1)},
		{"-Inf", math.Inf(-1)},
	} {
		b, err := json.Marshal(c.val)
		if err != nil {
			fmt.Printf("f64json\t%s\tERROR: %s\n", c.lit, err.Error())
			continue
		}
		fmt.Printf("f64json\t%s\t%s\n", c.lit, string(b))
	}
}
