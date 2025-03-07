package version

import (
	// "bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"time"
)

// Diff represents the difference between two file versions
type Diff struct {
	Filepath     string    `json:"filepath"`
	OldHash      string    `json:"old_hash"`
	NewHash      string    `json:"new_hash"`
	DiffSize     int64     `json:"diff_size"`
	ModifiedTime time.Time `json:"modified_time"`
	Operation    string    `json:"operation"` // "add", "modify", "delete"
}

// FileVersion represents a single version of a file
type FileVersion struct {
	ContentHash string `json:"content_hash"`
	Timestamp   int64  `json:"timestamp"`
	Size        int64  `json:"size"`
	Operation   string `json:"operation"`
}

// CASStorage is the content-addressable storage for file versions
type CASStorage struct {
	ObjectDir string
}

// NewCASStorage creates a new CAS storage instance
func NewCASStorage(repoPath string) *CASStorage {
	return &CASStorage{
		ObjectDir: repoPath + "/objects",
	}
}

// GetHash calculates the SHA256 hash of a file
func GetHash(filePath string) (string, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return "", err
	}
	defer file.Close()

	hasher := sha256.New()
	if _, err := io.Copy(hasher, file); err != nil {
		return "", err
	}

	return hex.EncodeToString(hasher.Sum(nil)), nil
}

// SaveFile saves a file to the content-addressable storage
func (cas *CASStorage) SaveFile(filePath string) (string, error) {
	hash, err := GetHash(filePath)
	if err != nil {
		return "", err
	}

	dest := cas.getObjectPath(hash)
	if _, err := os.Stat(dest); err == nil {
		// File already exists in storage
		return hash, nil
	}

	// Create directory if not exists
	if err := os.MkdirAll(cas.getObjectDir(hash), 0755); err != nil {
		return "", err
	}

	// Copy file to storage
	src, err := os.Open(filePath)
	if err != nil {
		return "", err
	}
	defer src.Close()

	dst, err := os.Create(dest)
	if err != nil {
		return "", err
	}
	defer dst.Close()

	if _, err := io.Copy(dst, src); err != nil {
		return "", err
	}

	return hash, nil
}

// CompareFiles compares two files and returns their differences
func CompareFiles(oldPath, newPath string) (*Diff, error) {
	oldStat, oldErr := os.Stat(oldPath)
	newStat, newErr := os.Stat(newPath)

	// Handle file deletion
	if errors.Is(oldErr, os.ErrNotExist) && newErr == nil {
		newHash, err := GetHash(newPath)
		if err != nil {
			return nil, err
		}

		return &Diff{
			Filepath:     newPath,
			OldHash:      "",
			NewHash:      newHash,
			DiffSize:     newStat.Size(),
			ModifiedTime: newStat.ModTime(),
			Operation:    "add",
		}, nil
	}

	// Handle file addition
	if errors.Is(newErr, os.ErrNotExist) && oldErr == nil {
		oldHash, err := GetHash(oldPath)
		if err != nil {
			return nil, err
		}

		return &Diff{
			Filepath:     oldPath,
			OldHash:      oldHash,
			NewHash:      "",
			DiffSize:     -oldStat.Size(),
			ModifiedTime: time.Now(),
			Operation:    "delete",
		}, nil
	}

	// Handle file modification
	if oldErr == nil && newErr == nil {
		oldHash, err := GetHash(oldPath)
		if err != nil {
			return nil, err
		}

		newHash, err := GetHash(newPath)
		if err != nil {
			return nil, err
		}

		if oldHash == newHash {
			return nil, nil // No changes
		}

		return &Diff{
			Filepath:     newPath,
			OldHash:      oldHash,
			NewHash:      newHash,
			DiffSize:     newStat.Size() - oldStat.Size(),
			ModifiedTime: newStat.ModTime(),
			Operation:    "modify",
		}, nil
	}

	return nil, fmt.Errorf("error comparing files: %v, %v", oldErr, newErr)
}

// getObjectDir returns the directory path for a given hash
func (cas *CASStorage) getObjectDir(hash string) string {
	return fmt.Sprintf("%s/%s/%s", cas.ObjectDir, hash[:2], hash[2:])
}

// getObjectPath returns the full path for a given hash
func (cas *CASStorage) getObjectPath(hash string) string {
	return fmt.Sprintf("%s/%s", cas.getObjectDir(hash), hash)
}

// ApplyDiff applies a diff to the storage
func (cas *CASStorage) ApplyDiff(diff *Diff) error {
	switch diff.Operation {
	case "add":
		if _, err := cas.SaveFile(diff.Filepath); err != nil {
			return err
		}
	case "delete":
		path := cas.getObjectPath(diff.OldHash)
		if err := os.Remove(path); err != nil {
			return err
		}
	case "modify":
		if _, err := cas.SaveFile(diff.Filepath); err != nil {
			return err
		}
	}
	return nil
}

// GenerateSnapshot generates a snapshot of the current state
func (cas *CASStorage) GenerateSnapshot(filePaths []string) ([]FileVersion, error) {
	var versions []FileVersion

	for _, filePath := range filePaths {
		hash, err := cas.SaveFile(filePath)
		if err != nil {
			return nil, err
		}

		stat, err := os.Stat(filePath)
		if err != nil {
			return nil, err
		}

		versions = append(versions, FileVersion{
			ContentHash: hash,
			Timestamp:   stat.ModTime().Unix(),
			Size:        stat.Size(),
			Operation:   "add",
		})
	}

	return versions, nil
}
