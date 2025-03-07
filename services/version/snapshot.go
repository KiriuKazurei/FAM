package version

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Snapshot represents a snapshot of the file system at a given point in time
type Snapshot struct {
	ID          string            `json:"id"`
	GeneratedAt time.Time         `json:"generated_at"`
	Files       []FileVersion     `json:"files"`
	Comments    map[string]string `json:"comments"`
}

// SnapshotManager manages the creation and storage of snapshots
type SnapshotManager struct {
	CASStorage *CASStorage
}

// NewSnapshotManager creates a new snapshot manager
func NewSnapshotManager(repoPath string) *SnapshotManager {
	return &SnapshotManager{
		CASStorage: NewCASStorage(repoPath),
	}
}

// GenerateSnapshot generates a new snapshot of the specified directory
func (sm *SnapshotManager) GenerateSnapshot(dirPath string) (*Snapshot, error) {
	files, err := sm.listFiles(dirPath)
	if err != nil {
		return nil, fmt.Errorf("failed to list files: %w", err)
	}

	var fileVersions []FileVersion
	comments := make(map[string]string)

	for _, file := range files {
		hash, err := sm.CASStorage.SaveFile(file)
		if err != nil {
			return nil, fmt.Errorf("failed to save file %s: %w", file, err)
		}

		fileInfo, err := os.Stat(file)
		if err != nil {
			return nil, fmt.Errorf("failed to get file info for %s: %w", file, err)
		}

		fileVersions = append(fileVersions, FileVersion{
			ContentHash: hash,
			Timestamp:   fileInfo.ModTime().Unix(),
			Size:        fileInfo.Size(),
			Operation:   "add",
		})

		// Placeholder for comments, can be populated based on user input
		comments[file] = ""
	}

	snapshot := &Snapshot{
		ID:          fmt.Sprintf("%d", time.Now().UnixNano()),
		GeneratedAt: time.Now(),
		Files:       fileVersions,
		Comments:    comments,
	}

	return snapshot, nil
}

// listFiles lists all files in the specified directory and its subdirectories
func (sm *SnapshotManager) listFiles(dirPath string) ([]string, error) {
	var files []string
	err := filepath.Walk(dirPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() {
			files = append(files, path)
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("failed to walk directory %s: %w", dirPath, err)
	}
	return files, nil
}

// SaveSnapshot saves a snapshot to the CAS storage
func (sm *SnapshotManager) SaveSnapshot(snapshot *Snapshot) error {
	snapshotPath := sm.CASStorage.getObjectPath(snapshot.ID)

	// Create directory if not exists
	if err := os.MkdirAll(filepath.Dir(snapshotPath), 0755); err != nil {
		return fmt.Errorf("failed to create directory for snapshot: %w", err)
	}

	// Serialize snapshot to JSON and save
	snapshotData, err := json.MarshalIndent(snapshot, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal snapshot: %w", err)
	}

	if err := os.WriteFile(snapshotPath, snapshotData, 0644); err != nil {
		return fmt.Errorf("failed to write snapshot to file: %w", err)
	}

	return nil
}

// LoadSnapshot loads a snapshot from the CAS storage
func (sm *SnapshotManager) LoadSnapshot(snapshotID string) (*Snapshot, error) {
	snapshotPath := sm.CASStorage.getObjectPath(snapshotID)

	snapshotData, err := os.ReadFile(snapshotPath)
	if err != nil {
		return nil, fmt.Errorf("failed to read snapshot file: %w", err)
	}

	var snapshot Snapshot
	if err := json.Unmarshal(snapshotData, &snapshot); err != nil {
		return nil, fmt.Errorf("failed to unmarshal snapshot: %w", err)
	}

	return &snapshot, nil
}

// ListSnapshots lists all available snapshots
func (sm *SnapshotManager) ListSnapshots() ([]string, error) {
	var snapshots []string
	snapshotDir := sm.CASStorage.ObjectDir

	err := filepath.Walk(snapshotDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() {
			snapshots = append(snapshots, filepath.Base(path))
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("failed to walk snapshot directory: %w", err)
	}

	return snapshots, nil
}
