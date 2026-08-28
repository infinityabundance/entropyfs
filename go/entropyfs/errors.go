package entropyfs

import (
	"encoding/json"
	"fmt"
)

// ErrorCode is the stable machine-readable error class (the C ABI's
// enum entropyfs_error; identical values). Programs switch on these;
// never parse Error.Message.
type ErrorCode uint32

const (
	CodeOK                 ErrorCode = 0
	CodeNotFound           ErrorCode = 1
	CodeInvalidArgument    ErrorCode = 2
	CodeCorruptStore       ErrorCode = 3
	CodeIncompatibleFormat ErrorCode = 4
	CodeResourceLimit      ErrorCode = 5
	CodeIO                 ErrorCode = 6
	CodeBusy               ErrorCode = 7
	CodeUnsupported        ErrorCode = 8
	CodeInternal           ErrorCode = 9
	CodeClosed             ErrorCode = 10
)

func (c ErrorCode) String() string {
	switch c {
	case CodeOK:
		return "OK"
	case CodeNotFound:
		return "NOT_FOUND"
	case CodeInvalidArgument:
		return "INVALID_ARGUMENT"
	case CodeCorruptStore:
		return "CORRUPT_STORE"
	case CodeIncompatibleFormat:
		return "INCOMPATIBLE_FORMAT"
	case CodeResourceLimit:
		return "RESOURCE_LIMIT"
	case CodeIO:
		return "IO"
	case CodeBusy:
		return "BUSY"
	case CodeUnsupported:
		return "UNSUPPORTED"
	case CodeInternal:
		return "INTERNAL"
	case CodeClosed:
		return "CLOSED"
	default:
		return fmt.Sprintf("UNKNOWN(%d)", uint32(c))
	}
}

// Error is the stable Go error: a machine-readable Code plus a
// diagnostic Message (which may improve over time — never parsed).
type Error struct {
	Code    ErrorCode
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("entropyfs: %s: %s", e.Code, e.Message)
}

// Is lets errors.Is(err, ErrNotFound) style checks work.
func (e *Error) Is(target error) bool {
	t, ok := target.(*Error)
	return ok && t.Code == e.Code
}

// Package sentinels for errors.Is.
var (
	ErrNotFound           = &Error{Code: CodeNotFound, Message: "not found"}
	ErrInvalidArgument    = &Error{Code: CodeInvalidArgument, Message: "invalid argument"}
	ErrCorruptStore       = &Error{Code: CodeCorruptStore, Message: "corrupt store"}
	ErrIncompatibleFormat = &Error{Code: CodeIncompatibleFormat, Message: "incompatible format"}
	ErrResourceLimit      = &Error{Code: CodeResourceLimit, Message: "resource limit"}
	ErrIO                 = &Error{Code: CodeIO, Message: "i/o error"}
	ErrBusy               = &Error{Code: CodeBusy, Message: "busy"}
	ErrUnsupported        = &Error{Code: CodeUnsupported, Message: "unsupported"}
	ErrInternal           = &Error{Code: CodeInternal, Message: "internal error"}
	ErrClosed             = &Error{Code: CodeClosed, Message: "engine is closed"}
)

// parseMetrics parses the native EngineMetrics JSON DTO into the stable
// typed subset, preserving the full raw document.
func parseMetrics(raw []byte) (Metrics, error) {
	var doc struct {
		SchemaVersion uint32 `json:"schema_version"`
		Accounting    struct {
			LogicalBytes          uint64 `json:"logical_bytes"`
			ReachableBytes        uint64 `json:"reachable_bytes"`
			PhysicalUsedBytes     uint64 `json:"physical_used_bytes"`
			PhysicalCapacityBytes uint64 `json:"physical_capacity_bytes"`
			PhysicalFreeBytes     uint64 `json:"physical_free_bytes"`
			ObjectCount           uint64 `json:"object_count"`
			DataRecordCount       uint64 `json:"data_record_count"`
			BlobCount             uint64 `json:"blob_count"`
		} `json:"accounting"`
		Pressure struct {
			Pressured            bool   `json:"pressured"`
			Samples              uint64 `json:"samples"`
			EnterEvents          uint64 `json:"enter_events"`
			LeaveEvents          uint64 `json:"leave_events"`
			PressuredTimeMs      uint64 `json:"pressured_time_ms"`
			RansSkips            uint64 `json:"rans_skips"`
			DeferredExtents      uint64 `json:"deferred_extents"`
			DeferredLogicalBytes uint64 `json:"deferred_logical_bytes"`
			DeferredAgeMs        uint64 `json:"deferred_age_ms"`
			PeakDeferredBytes    uint64 `json:"peak_deferred_bytes"`
			DebtCapEngagements   uint64 `json:"debt_cap_engagements"`
		} `json:"pressure"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return Metrics{}, fmt.Errorf("%w: metrics JSON: %v", ErrInternal, err)
	}
	return Metrics{
		SchemaVersion: doc.SchemaVersion,
		Accounting: AccountingMetrics{
			LogicalBytes:          doc.Accounting.LogicalBytes,
			ReachableBytes:        doc.Accounting.ReachableBytes,
			PhysicalUsedBytes:     doc.Accounting.PhysicalUsedBytes,
			PhysicalCapacityBytes: doc.Accounting.PhysicalCapacityBytes,
			PhysicalFreeBytes:     doc.Accounting.PhysicalFreeBytes,
			ObjectCount:           doc.Accounting.ObjectCount,
			DataRecordCount:       doc.Accounting.DataRecordCount,
			BlobCount:             doc.Accounting.BlobCount,
		},
		Pressure: PressureMetrics{
			Pressured:            doc.Pressure.Pressured,
			Samples:              doc.Pressure.Samples,
			EnterEvents:          doc.Pressure.EnterEvents,
			LeaveEvents:          doc.Pressure.LeaveEvents,
			PressuredTimeMs:      doc.Pressure.PressuredTimeMs,
			RansSkips:            doc.Pressure.RansSkips,
			DeferredExtents:      doc.Pressure.DeferredExtents,
			DeferredLogicalBytes: doc.Pressure.DeferredLogicalBytes,
			DeferredAgeMs:        doc.Pressure.DeferredAgeMs,
			PeakDeferredBytes:    doc.Pressure.PeakDeferredBytes,
			DebtCapEngagements:   doc.Pressure.DebtCapEngagements,
		},
		Raw: raw,
	}, nil
}
