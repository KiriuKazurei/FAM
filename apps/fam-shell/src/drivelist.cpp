#include "drivelist.h"

#include <QListWidget>
#include <QVBoxLayout>

#include <windows.h>

#include "famlistmodel.h"

namespace {

QString kindOf(UINT type) {
    switch (type) {
    case DRIVE_REMOVABLE:
        return QStringLiteral("Removable");
    case DRIVE_FIXED:
        return QStringLiteral("Local");
    case DRIVE_REMOTE:
        return QStringLiteral("Network");
    case DRIVE_CDROM:
        return QStringLiteral("CD");
    case DRIVE_RAMDISK:
        return QStringLiteral("RAM");
    default:
        return QStringLiteral("Unknown");
    }
}

/* "C:\" for the Win32 calls, which document a trailing backslash, and "C:/" for the core, which
 * spells separators the way it stores them. Handing either side the other's form is the kind of bug
 * that shows up as an empty drive rather than an error. */
QString winRoot(QChar letter) {
    return QString(letter) + QStringLiteral(":\\");
}

QString coreRoot(QChar letter) {
    return QString(letter) + QStringLiteral(":/");
}

const WCHAR *native(const QString &path) {
    /* QString already holds UTF-16, so this is a cast rather than a conversion. */
    return reinterpret_cast<const WCHAR *>(path.utf16());
}

} /* namespace */

void DriveWorker::run() {
    QVector<DriveInfo> drives;
    const DWORD mask = GetLogicalDrives();
    for (int bit = 0; bit < 26; ++bit) {
        if ((mask & (1u << bit)) == 0) {
            continue;
        }
        /* Checked per drive, not per batch of syscalls: the shutdown path waits for this thread, and
         * one hung volume should not cost more than one call's worth of waiting. */
        if (isInterruptionRequested()) {
            return;
        }
        const QChar letter = QChar(u'A' + bit);
        const QString win = winRoot(letter);
        DriveInfo info;
        info.root = coreRoot(letter);
        const UINT type = GetDriveTypeW(native(win));
        info.kind = kindOf(type);
        /* A mapped share whose server is gone answers `GetDriveTypeW` from the redirector's cache and
         * then blocks `GetDiskFreeSpaceExW` for as long as the SMB timeout says. Capacity is the
         * least interesting thing about a network drive anyway, so it is not asked for. */
        if (type == DRIVE_REMOTE) {
            info.reachable = true;
        } else {
            ULARGE_INTEGER freeAvailable = {};
            ULARGE_INTEGER total = {};
            ULARGE_INTEGER totalFree = {};
            info.reachable = GetDiskFreeSpaceExW(native(win), &freeAvailable, &total, &totalFree)
                != 0;
            if (info.reachable) {
                info.freeBytes = freeAvailable.QuadPart;
                info.totalBytes = total.QuadPart;
            }
        }
        drives.append(info);
    }
    emit ready(drives);
}

DriveList::DriveList(QWidget *parent) : QWidget(parent) {
    qRegisterMetaType<QVector<DriveInfo>>();
    m_list = new QListWidget(this);
    m_list->setUniformItemSizes(true);
    m_list->setSelectionMode(QAbstractItemView::SingleSelection);
    m_list->setMaximumWidth(220);

    auto *box = new QVBoxLayout(this);
    box->setContentsMargins(0, 0, 4, 0);
    box->addWidget(m_list);

    connect(m_list, &QListWidget::currentItemChanged, this,
            [this](QListWidgetItem *current, QListWidgetItem *) {
                if (current == nullptr) {
                    return;
                }
                const QString root = current->data(Qt::UserRole).toString();
                if (!root.isEmpty()) {
                    emit drivePicked(root);
                }
            });
    connect(&m_worker, &DriveWorker::ready, this, &DriveList::adopt);
    m_worker.start();
}

DriveList::~DriveList() {
    /* A QThread must not be destroyed while running, so this waits - and the wait is only as bounded
     * as one Win32 call. Skipping the capacity question for network drives removes the known hang,
     * but `GetDriveTypeW` itself goes through the redirector for a dead mapped share, and this
     * machine has no network drive to measure that on. So: not "bounded by design", just bounded by
     * one call, untested for the case that would actually block. */
    m_worker.requestInterruption();
    m_worker.wait();
}

void DriveList::adopt(const QVector<DriveInfo> &drives) {
    m_drives = drives;
    m_list->clear();
    for (const DriveInfo &drive : m_drives) {
        QString text = QStringLiteral("%1  %2").arg(drive.root.left(2), drive.kind);
        if (!drive.reachable) {
            text += QStringLiteral(" · not reachable");
        } else if (drive.totalBytes > 0) {
            text += QStringLiteral(" · %1 of %2 free")
                        .arg(formatSize(drive.freeBytes), formatSize(drive.totalBytes));
        }
        auto *item = new QListWidgetItem(text, m_list);
        if (drive.reachable) {
            item->setData(Qt::UserRole, drive.root);
        } else {
            /* Selectable but with nothing to open: a drive that cannot be answered about itself is
             * shown so the reader knows it exists, and choosing it would open a folder that is not
             * there. */
            item->setFlags(item->flags() & ~Qt::ItemIsEnabled);
        }
    }
}

QString DriveList::selectFirst() {
    for (int row = 0; row < m_drives.size(); ++row) {
        if (!m_drives.at(row).reachable) {
            continue;
        }
        m_list->setCurrentRow(row);
        return m_drives.at(row).root;
    }
    return QString();
}
