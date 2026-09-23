#ifndef DRIVELIST_H
#define DRIVELIST_H

#include <QThread>
#include <QString>
#include <QVector>
#include <QWidget>

class QListWidget;

/* One volume as the shell sees it. `reachable` is false when the very first question about the drive
 * could not be answered, which is what a mapped-but-dead network share looks like. */
struct DriveInfo {
    QString root; /* "C:\", spelled the way the core wants a path */
    QString kind; /* Local, Removable, Network, CD, RAM, or Unknown */
    quint64 totalBytes = 0;
    quint64 freeBytes = 0;
    bool reachable = false;
};

Q_DECLARE_METATYPE(QVector<DriveInfo>)

/* The volume list, built off the GUI thread.
 *
 * This is not a formality. `GetDiskFreeSpaceExW` on a mapped network drive whose server is gone can
 * block for seconds, and the shell's whole promise is that nothing on the paint path waits for the
 * filesystem - the same promise that keeps directory reads on the core's loader thread. So the list
 * arrives when it arrives, and the sidebar says "drives" until then. */
class DriveWorker final : public QThread {
    Q_OBJECT

public:
    using QThread::QThread;

signals:
    void ready(const QVector<DriveInfo> &drives);

protected:
    void run() override;
};

class DriveList final : public QWidget {
    Q_OBJECT

public:
    explicit DriveList(QWidget *parent = nullptr);
    ~DriveList() override;

    /* Picks the first reachable drive, as the headless gate does: a sidebar nobody clicked is a
     * sidebar whose whole job - turning a volume into a column - has never run. Returns the root, or
     * empty when nothing is there yet. */
    QString selectFirst();

signals:
    void drivePicked(const QString &root);

private slots:
    void adopt(const QVector<DriveInfo> &drives);

private:
    QListWidget *m_list = nullptr;
    DriveWorker m_worker;
    QVector<DriveInfo> m_drives;
};

#endif /* DRIVELIST_H */
