pipeline {
    environment {
        DOCKER_REGISTRY_URL = "10.123.31.221:5000"
        KEEP_LAST_N_IMAGES = 3
        // SLACK_NOTIFICATION_CHANNEL = "avacuscc-jenkins-notification-dev"


        SERVICE_GIT_REPO = "https://github.com/wakumo/temper.git"
        SERVICE_GIT_BRANCH = "main"
        //SERVICE_NAMESPACE = "wakumo"
        SERVICE_NAME = "temper"
        SERVICE_IMAGE_NAME = "${DOCKER_REGISTRY_URL}/${SERVICE_NAME}:${BUILD_NUMBER}"


        CONFIG_PROJECT_NAME = "wakumo-development-k8s"
        CONFIG_REPO_URL = "github.com/wakumo/wakumo-development-k8s.git"
        CONFIG_DEPLOYMENT_FILE = "namespaces/ethereum-tx/temper/resources/temper-deployment.yml"
    }
    agent any

    stages {
        stage('Checkout Service') {
            when {
                branch 'main'
            }
            steps {
                git branch: "${SERVICE_GIT_BRANCH}", credentialsId: 'github-develop', url: "${SERVICE_GIT_REPO}"
            }
        }
        stage('Build Service Image') {
            steps {
                script {
                    Image = docker.build("${SERVICE_IMAGE_NAME}", "--no-cache .")
                    // push image to registry
                    docker.withRegistry("http://${DOCKER_REGISTRY_URL}") {
                        Image.push()
                    }
                }
            }
        }

        stage('Update Deployment Config File') {
            steps {
                script {
                    withCredentials([usernamePassword(credentialsId: 'github-develop',
                                                    usernameVariable: 'GIT_USERNAME',
                                                    passwordVariable: 'GIT_PASSWORD')]) {
                        sh """
                            # Xóa thư mục nếu tồn tại
                            rm -rf wakumo-development-k8s || true

                            # Clone repository với credentials
                            git clone -b main https://${GIT_USERNAME}:${GIT_PASSWORD}@${CONFIG_REPO_URL}
                            cd ${CONFIG_PROJECT_NAME}

                            # Configure git
                            git config user.name "Jenkins CI"
                            git config user.email "jenkins@wakumo.vn"

                            # Update image tag in deployment file
                            sed -i 's|image: .*${SERVICE_NAME}:.*|image: ${SERVICE_IMAGE_NAME}|' ${CONFIG_DEPLOYMENT_FILE}

                            # Commit and push changes
                            git add ${CONFIG_DEPLOYMENT_FILE}
                            git commit -m "Update image tag to version ${BUILD_NUMBER}"
                            git push

                            # Cleanup
                            cd ..
                            rm -rf ${CONFIG_PROJECT_NAME}
                        """
                    }
                }
            }
        }
        stage('Cleanup Old Images') {
            steps {
                script {
                    sh """
                        # Lấy danh sách tags và sắp xếp theo số
                        ALL_TAGS=\$(docker images \${DOCKER_REGISTRY_URL}/\${SERVICE_NAME} --format '{{.Tag}}' | sort -rn)

                        # Đếm số lượng tags
                        TAG_COUNT=\$(echo "\$ALL_TAGS" | wc -l)

                        if [ \$TAG_COUNT -gt ${KEEP_LAST_N_IMAGES} ]; then
                            # Lấy danh sách các tags cần xóa (bỏ qua 3 tags mới nhất)
                            TAGS_TO_DELETE=\$(echo "\$ALL_TAGS" | tail -n +\$((${KEEP_LAST_N_IMAGES} + 1)))

                            for tag in \$TAGS_TO_DELETE; do
                                echo "Deleting image tag: \$tag"
                                docker rmi "\${DOCKER_REGISTRY_URL}/\${SERVICE_NAME}:\$tag" || true
                            done

                            # Dọn dẹp các image không còn sử dụng
                            docker image prune -f
                        fi
                    """
                }
            }
        }
    }
    post {
       // only triggered when blue or green sign
       success {
           slackSend channel: "${SLACK_NOTIFICATION_CHANNEL}", message: "`${SERVICE_NAME}` has completed the build image with commit `${GIT_COMMIT}` .\n Image: `${SERVICE_IMAGE_NAME}`", color: '#1ddb46'
       }
       // triggered when red sign
       failure {
           slackSend channel: "${SLACK_NOTIFICATION_CHANNEL}", message: "`${SERVICE_NAME}` has built an image of failure with commit `${GIT_COMMIT}`. please try again!!!", color: '#FE2E2E'
       }
    }

}