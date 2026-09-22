#include "iconcache.h"

#include <QApplication>
#include <QPixmap>
#include <QStyle>

#include <windows.h>

#include <commctrl.h>
#include <commoncontrols.h>
#include <shellapi.h>

namespace {

/* The pixel size of a small shell icon on this machine, read rather than assumed: it is 16 on every
 * desktop scaling this shell will meet and 32 on the ones it will not, and an icon drawn at the
 * wrong size is a blurry row. */
int smallIconSize() {
    const int size = GetSystemMetrics(SM_CXSMICON);
    return size > 0 ? size : 16;
}

/* HICON to QImage with nothing from Qt's GUI half: a top-down 32-bit DIB, the icon drawn into it,
 * the bits copied out. This is the only place an HICON becomes pixels, and it runs on the worker,
 * which is what keeps QImage - the one pixel container Qt allows off the GUI thread - the only
 * thing that ever crosses the thread boundary. */
QImage imageFromIcon(HICON icon, int size) {
    QImage image(size, size, QImage::Format_ARGB32_Premultiplied);
    image.fill(Qt::transparent);
    BITMAPINFO info{};
    info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    info.bmiHeader.biWidth = size;
    info.bmiHeader.biHeight = -size; /* top-down, so the DIB's rows are QImage's rows */
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    void *bits = nullptr;
    HBITMAP dib = CreateDIBSection(nullptr, &info, DIB_RGB_COLORS, &bits, nullptr, 0);
    if (dib == nullptr || bits == nullptr) {
        return image;
    }
    const HDC dc = CreateCompatibleDC(nullptr);
    const HGDIOBJ previous = SelectObject(dc, dib);
    DrawIconEx(dc, 0, 0, icon, size, size, 0, nullptr, DI_NORMAL);
    memcpy(image.bits(), bits, size_t(size) * size_t(size) * 4);
    SelectObject(dc, previous);
    DeleteDC(dc);
    DeleteObject(dib);
    return image;
}

/* Tier 1: the extension table. SHGFI_USEFILEATTRIBUTES is the whole trick - with it set the shell
 * answers about the name's shape and the attributes, and never opens the path string, so this costs
 * a hash lookup's worth of shell work and no I/O at all. */
QImage resolveByExtension(const IconKey &key, IImageList *list, std::atomic<int> &extracted,
                          std::atomic<int> &destroyed) {
    if (list == nullptr) {
        return {};
    }
    /* A name with no dot asks for the plain file icon; a directory asks for the folder icon. Both
     * are spelled as dot-less probes so the extension parser has nothing to misread. */
    const QString probe = key.directory ? QStringLiteral("Directory")
                          : key.suffix.isEmpty() ? QStringLiteral("File") : key.suffix;
    const DWORD attrs = key.attrs != 0 ? key.attrs
                        : key.directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
    SHFILEINFOW info{};
    SHGetFileInfoW(reinterpret_cast<const WCHAR *>(probe.constData()), attrs, &info, sizeof(info),
                   SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES);
    HICON icon = nullptr;
    if (FAILED(list->GetIcon(info.iIcon, ILD_TRANSPARENT, &icon)) || icon == nullptr) {
        return {};
    }
    ++extracted;
    const QImage image = imageFromIcon(icon, smallIconSize());
    DestroyIcon(icon);
    ++destroyed;
    return image;
}

/* Tier 2: the names whose icon is not in their extension, resolved from the real path and therefore
 * costing one path lookup each. Links are followed, `desktop.ini` is honoured, and overlays - the
 * little arrow on a shortcut, the hand on a share - are composited in by the shell itself. */
QImage resolveByPath(const IconKey &key, IImageList *list, std::atomic<int> &extracted,
                     std::atomic<int> &destroyed) {
    if (key.directory) {
        /* A directory without a desktop.ini has nothing a real-path read could add over the folder
         * icon, and the read is the expensive part: one path lookup on this machine costs about what
         * re-reading a small directory does. Probe first, and only pay the read when the folder
         * actually customises itself. */
        const QString ini = key.path + QLatin1String("\\desktop.ini");
        if (GetFileAttributesW(reinterpret_cast<const WCHAR *>(ini.constData())) ==
            INVALID_FILE_ATTRIBUTES) {
            return resolveByExtension(key, list, extracted, destroyed);
        }
    }
    SHFILEINFOW info{};
    const ULONG_PTR result = SHGetFileInfoW(
        reinterpret_cast<const WCHAR *>(key.path.constData()), 0, &info, sizeof(info),
        SHGFI_ICON | SHGFI_ADDOVERLAYS | SHGFI_SMALLICON);
    if (result == 0 || info.hIcon == nullptr) {
        return resolveByExtension(key, list, extracted, destroyed);
    }
    ++extracted;
    const QImage image = imageFromIcon(info.hIcon, smallIconSize());
    DestroyIcon(info.hIcon);
    ++destroyed;
    return image;
}

} /* namespace */

IconWorker::IconWorker(QObject *parent) : QThread(parent) {}

IconWorker::~IconWorker() {
    stop();
    wait();
}

void IconWorker::request(const QVector<IconKey> &keys) {
    {
        QMutexLocker locker(&m_lock);
        m_pending.append(keys);
    }
    m_wake.wakeAll();
}

void IconWorker::stop() {
    {
        QMutexLocker locker(&m_lock);
        m_stop = true;
    }
    m_wake.wakeAll();
}

void IconWorker::run() {
    /* STA, because the shell namespace is COM, and on this thread alone: the first SHGetImageList
     * builds the system image list, which is a 100ms-to-seconds stall on a machine with many shell
     * handlers registered. All three sizes are warmed here so no later view pays for the build
     * mid-scroll. */
    const HRESULT com = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    IImageList *small = nullptr;
    IImageList *large = nullptr;
    IImageList *jumbo = nullptr;
    SHGetImageList(SHIL_SYSSMALL, __uuidof(IImageList), reinterpret_cast<void **>(&small));
    SHGetImageList(SHIL_EXTRALARGE, __uuidof(IImageList), reinterpret_cast<void **>(&large));
    SHGetImageList(SHIL_JUMBO, __uuidof(IImageList), reinterpret_cast<void **>(&jumbo));

    for (;;) {
        QVector<IconKey> batch;
        {
            QMutexLocker locker(&m_lock);
            while (m_pending.isEmpty() && !m_stop) {
                m_wake.wait(&m_lock);
            }
            if (m_pending.isEmpty()) {
                break;
            }
            batch.swap(m_pending);
        }
        QVector<IconResult> results;
        results.reserve(batch.size());
        for (const IconKey &key : batch) {
            QImage image = key.tier2 ? resolveByPath(key, small, m_extracted, m_destroyed)
                                     : resolveByExtension(key, small, m_extracted, m_destroyed);
            if (image.isNull()) {
                continue;
            }
            results.append(IconResult{key, image});
        }
        if (!results.isEmpty()) {
            emit resolved(results);
        }
    }

    if (small != nullptr) {
        small->Release();
    }
    if (large != nullptr) {
        large->Release();
    }
    if (jumbo != nullptr) {
        jumbo->Release();
    }
    if (SUCCEEDED(com)) {
        CoUninitialize();
    }
}

IconCache::IconCache(QObject *parent) : QObject(parent) {
    qRegisterMetaType<QVector<IconResult>>();
    connect(&m_worker, &IconWorker::resolved, this, &IconCache::store);
    m_worker.start();
}

IconCache::~IconCache() {
    m_worker.stop();
    m_worker.wait();
}

QIcon IconCache::iconFor(const IconKey &key) {
    const auto hit = m_icons.constFind(key);
    if (hit != m_icons.constEnd()) {
        return *hit;
    }
    if (!m_requested.contains(key)) {
        m_requested.insert(key, true);
        m_worker.request(QVector<IconKey>{key});
    }
    /* The placeholder is the style's own glyph, not a shell call: it has to be free, because it is
     * what every row paints with until the worker has been past it once. */
    if (m_filePlaceholder.isNull()) {
        const QStyle *style = QApplication::style();
        m_filePlaceholder = style->standardIcon(QStyle::SP_FileIcon);
        m_dirPlaceholder = style->standardIcon(QStyle::SP_DirIcon);
    }
    return key.directory ? m_dirPlaceholder : m_filePlaceholder;
}

int IconCache::cached() const {
    return m_icons.size();
}

void IconCache::store(const QVector<IconResult> &results) {
    for (const IconResult &result : results) {
        /* store() runs on the GUI thread - it is a slot reached by a queued signal - which is the
         * one place a QPixmap may be built, and an icon is a pixmap cache. */
        m_icons.insert(result.key, QIcon(QPixmap::fromImage(result.image)));
    }
    /* Bounded on purpose: a few hundred extensions is the whole of tier 1, and tier 2 paths are
     * viewport-bound, so hitting this means something is browsing far wider than a screen and the
     * honest answer is to start over rather than grow. */
    if (m_icons.size() > 512) {
        m_icons.clear();
        m_requested.clear();
    }
    emit resolved();
}

IconKey iconKeyFor(const QString &name, const QString &path, quint32 attrs, bool directory) {
    IconKey key;
    key.attrs = attrs;
    key.directory = directory;
    const int dot = name.lastIndexOf(QLatin1Char('.'));
    if (dot > 0 && dot < name.size() - 1) {
        key.suffix = name.mid(dot).toLower();
    }
    /* The names whose icon is not a function of their extension: links, which point elsewhere;
     * drive roots, whose icon is the volume's; and directories, whose desktop.ini may say so. */
    bool tier2 = directory;
    if (key.suffix == QLatin1String(".lnk") || key.suffix == QLatin1String(".url") ||
        key.suffix == QLatin1String(".library-ms")) {
        tier2 = true;
    }
    if (!directory && name.size() >= 2 && name.at(1) == QLatin1Char(':') && name.size() <= 3) {
        tier2 = true;
    }
    key.tier2 = tier2;
    if (tier2) {
        key.path = path;
    }
    return key;
}
