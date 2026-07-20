// Package protocol defines the versioned local taskcaged wire model.
package protocol

const Version = 1

type RunRequest struct {
	Type            string            `json:"type"`
	ProtocolVersion int               `json:"protocolVersion"`
	JobID           string            `json:"jobId"`
	Command         []string          `json:"command"`
	WorkingDir      string            `json:"workingDirectory,omitempty"`
	Environment     map[string]string `json:"environment,omitempty"`
	Budget          ResourceBudget    `json:"budget"`
}

type ResourceBudget struct {
	MemoryBytes      int64 `json:"memoryBytes"`
	CPUQuotaMicros   int64 `json:"cpuQuotaMicros"`
	CPUPeriodMicros  int64 `json:"cpuPeriodMicros"`
	MaxProcesses     int64 `json:"maxProcesses"`
	WallTimeNanos    int64 `json:"wallTimeNanos"`
	MaxOutputBytes   int64 `json:"maxOutputBytes"`
}

type ExecutionResult struct {
	Type            string         `json:"type"`
	ProtocolVersion int            `json:"protocolVersion"`
	JobID           string         `json:"jobId"`
	Status          string         `json:"status"`
	Reason          string         `json:"reason"`
	ExitCode        *int           `json:"exitCode,omitempty"`
	QueueTimeNanos  int64          `json:"queueTimeNanos"`
	WallTimeNanos   int64          `json:"wallTimeNanos"`
	CPUTimeMicros   int64          `json:"cpuTimeMicros"`
	PeakMemoryBytes int64          `json:"peakMemoryBytes"`
	PeakProcesses   int64          `json:"peakProcesses"`
	Stdout          CapturedOutput `json:"stdout"`
	Stderr          CapturedOutput `json:"stderr"`
}

type CapturedOutput struct {
	DataBase64 string `json:"dataBase64"`
	Truncated  bool   `json:"truncated"`
}
