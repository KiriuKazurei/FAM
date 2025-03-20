package image

import (
	"os"
	"path/filepath"
	"fmt"
	"strings"
	"image"
)

// 添加libraw的CGO依赖
/*
#cgo pkg-config: libraw
#include <libraw.h>
#include <stdlib.h>
*/
import "C"
import (
	"unsafe"
	"errors"
)

func getRawMetadata(filePath string) (map[string]interface{}, error) {
	processor := C.libraw_init(0)
	defer C.libraw_close(processor)

	cPath := C.CString(filePath)
	defer C.free(unsafe.Pointer(cPath))

	ret := C.libraw_open_file(processor, cPath)
	if ret != 0 {
		return nil, errors.New("无法打开RAW文件")
	}

	ret = C.libraw_unpack(processor)
	if ret != 0 {
		return nil, errors.New("无法解包RAW数据")
	}

	idata := processor.idata
	other := processor.other

	meta := make(map[string]interface{})
	meta["camera_make"] = C.GoString(&idata.make[0])
	meta["camera_model"] = C.GoString(&idata.model[0])
	meta["iso_speed"] = int(other.iso_speed)
	meta["shutter_speed"] = float64(other.shutter)
	meta["focal_length"] = float64(other.focal_len)
	meta["exposure_time"] = float64(other.exposure_time)
	meta["aperture"] = float64(other.aperture)
	meta["white_balance"] = fmt.Sprintf("%v,%v", other.color[0], other.color[1])

	return meta, nil
}

// ProcessImage 返回结构化的元数据（新增字段说明）
func ProcessImage(filePath string) (map[string]interface{}, error) {
	fileInfo, err := os.Stat(filePath)
	if err != nil {
		return nil, fmt.Errorf("无法读取文件: %v", err)
	}

	ext := filepath.Ext(filePath)
	isRaw := isRawFormat(ext)

	meta := make(map[string]interface{})
	meta["file_size"] = fileInfo.Size()
	meta["file_modified"] = fileInfo.ModTime().Unix() // 新增文件修改时间

	if isRaw {
		rawMeta, err := getRawMetadata(filePath)
		if err != nil {
			return nil, err
		}
		meta["raw_metadata"] = rawMeta
		meta["camera_info"] = fmt.Sprintf("%v %v", rawMeta["camera_make"], rawMeta["camera_model"]) // 合并相机信息
	} else {
		stdMeta, err := getStandardMetadata(filePath)
		if err != nil {
			return nil, err
		}
		meta["standard_metadata"] = stdMeta
		meta["resolution"] = fmt.Sprintf("%dx%d", stdMeta["width"], stdMeta["height"]) // 新增分辨率字段
	}

	return meta, nil
}

// isRawFormat 检查文件扩展名是否为RAW格式
func isRawFormat(ext string) bool {
	// 常见RAW格式扩展名列表（小写形式）
	rawExtensions := map[string]struct{}{
		".cr2": {}, // Canon
		".nef": {}, // Nikon
		".arw": {}, // Sony
		".raf": {}, // Fujifilm
		".dng": {}, // Adobe RAW
	}

	// 统一转为小写后检查
	ext = strings.ToLower(ext)
	_, exists := rawExtensions[ext]
	return exists
}

// 新增getStandardMetadata函数
func getStandardMetadata(filePath string) (map[string]interface{}, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("无法打开文件: %w", err)
	}
	defer file.Close()

	config, format, err := image.DecodeConfig(file)
	if err != nil {
		return nil, fmt.Errorf("无法解码图像配置: %w", err)
	}

	meta := make(map[string]interface{})
	meta["width"] = config.Width
	meta["height"] = config.Height
	meta["format"] = format
	meta["bit_depth"] = config.BitDepth // 确保位深度正确返回

	return meta, nil
}
